<p align="center"><img src="assets/sofabuffers_logo.png" alt="SofaBuffers" height="140"></p>

# SofaBuffers

<b>Structured Objects For Anyone</b><br>
<i>... so optimized, feels amazing.</i>

[Would you like to know more?](https://github.com/sofa-buffers)

## SofaBuffers Rust library (`no_std`)

[![CI](https://github.com/sofa-buffers/corelib-rs-no-std/actions/workflows/ci.yml/badge.svg)](https://github.com/sofa-buffers/corelib-rs-no-std/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fsofa-buffers%2Fcorelib-rs-no-std%2Fbadges%2Fcoverage.json)](https://github.com/sofa-buffers/corelib-rs-no-std/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-1f7feb)](https://sofa-buffers.github.io/corelib-rs-no-std/)

[GitHub repository](https://github.com/sofa-buffers/corelib-rs-no-std)

A `#![no_std]`, **heap-free**, **streaming** Rust implementation of the
SofaBuffers (*Sofab*) serialization format. It is a port of the C `corelib`
(`istream.c` / `ostream.c`) and runs on any platform, from tiny
microcontrollers to desktops and servers. The whole crate is
`#![forbid(unsafe_code)]`, allocates nothing, and keeps every byte of state in
caller-provided buffers and structs — so it links into firmware where an
allocator (and the `std`-based [`corelib-rs`](https://github.com/sofa-buffers/corelib-rs))
cannot go.

This library implements SofaBuffers **API version 1** (exposed as
`sofab::API_VERSION`). The wire format is specified, language-neutrally, in the
[SofaBuffers documentation](https://github.com/sofa-buffers/documentation); the
test suite replays the **shared** cross-language vectors
([`assets/test_vectors.json`](assets/test_vectors.json), copied verbatim from
the documentation repo) to guarantee byte-for-byte interoperability with every
other language port.

**Requirements:** Rust **1.70+** (the crate's MSRV), edition 2021, stable
toolchain. Builds on any target, including bare-metal `thumbv6m` / `thumbv7em` /
`riscv32imc` with no host `std`.

**Dependencies:** none. The library pulls in **zero runtime crates** — only
`core` (no `alloc`). `libc` and `serde_json` appear solely as
`dev-dependencies` for the benchmarks and the conformance test suite.

**Package name:** the crates.io package is `sofa-buffers-corelib-no-std`; the
compiled crate (what you `use`) is `sofab`.

```bash
cargo add sofa-buffers-corelib-no-std
```

## Why this design

| Goal | How |
|------|-----|
| No allocator | All state lives in caller-provided buffers/structs. Nothing is ever boxed. |
| No `unsafe` | The crate is `#![forbid(unsafe_code)]`. Endianness is handled with `to_le_bytes`/`from_le_bytes`. |
| Streaming **out** | [`OStream`] writes into a small caller buffer and calls a [`Flush`] sink whenever it fills, so a message can exceed RAM. |
| Streaming **in** | [`IStream`] is a byte-at-a-time state machine fed arbitrary chunks; large string/blob payloads are delivered in pieces. |
| Reserve-offset | `OStream::with_offset` leaves room at the front of the buffer for a lower-layer protocol header (saves a copy). |
| Small footprint | Cargo features drop whole code paths; size-optimized release profile (`opt-level="z"`, LTO, `panic="abort"`). |

## Usage

Every example below compiles against the real API and needs no allocator.

### Simple encode

```rust
use sofab::OStream;

let mut buf = [0u8; 64];                 // caller-owned, fixed capacity
let used = {
    let mut os = OStream::new(&mut buf); // borrows buf for its lifetime
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "hi").unwrap();
    os.bytes_used()                      // bytes written so far
};
let wire = &buf[..used];
```

### Simple decode

Decoding is **push-based**: you implement [`Visitor`] and the decoder calls back
the methods for the field kinds you care about. Any method you leave at its
default (empty) body transparently **skips** that field.

```rust
use sofab::{IStream, Visitor, Id, Unsigned, Signed};

#[derive(Default)]
struct My { a: u64, b: i64 }
impl Visitor for My {
    fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
    fn signed(&mut self, id: Id, v: Signed)     { if id == 2 { self.b = v; } }
    // string(), blob(), fp32(), array_begin(), sequence_begin(), ... as needed
}

let mut sink = My::default();
IStream::new().feed(wire, &mut sink).unwrap();
```

### Streaming a message larger than the buffer — the `OStream` output primitive

[`OStream`] is the streaming *output* primitive. Give it a **tiny** window and a
[`Flush`] sink (any `FnMut(&[u8])`, or a manual `impl Flush` on bare metal); when
the window fills it drains to the sink and keeps going, so the produced message
can be far larger than RAM.

```rust
use sofab::OStream;

let mut scratch = [0u8; 16];                 // tiny window, not the whole message
let mut out = Vec::new();                     // or a UART / socket / flash page
{
    let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| {
        out.extend_from_slice(chunk);         // called every time the window fills
    });
    for i in 0..1000u32 {
        os.write_unsigned(i, i as u64).unwrap();
    }
    os.flush();                               // push the final partial window
}
```

`OStream::with_offset` reserves header bytes at the front of the buffer, and
`OStream::buffer_set` swaps in a fresh buffer mid-stream (typically from inside
the flush sink).

### The `IStream` input primitive — chunked feeding

[`IStream`] is the streaming *input* primitive: a byte-at-a-time state machine
that resumes at any boundary, so you can feed it arbitrarily small chunks as they
arrive off the wire. String/blob payloads are delivered to the visitor in pieces.

```rust
use sofab::{IStream, Visitor, Id};

#[derive(Default)]
struct Len { total: usize }
impl Visitor for Len {
    fn blob(&mut self, _id: Id, total: usize, _offset: usize, chunk: &[u8]) {
        self.total = total;                   // `chunk` borrows the fed bytes
        // copy `chunk` out here if you need it after this call returns
        let _ = chunk;
    }
}

let mut sink = Len::default();
let mut is = IStream::new();
for piece in wire.chunks(4) {                 // one packet at a time
    is.feed(piece, &mut sink).unwrap();
}
```

### Driving generated object code

The most common real use case is a schema compiled by **`sofabgen`** into typed
structs whose `encode` / `decode` methods drive this runtime. This crate ships
the *runtime*; generated code calls it exactly like the pattern below (fixed
buffers, no heap):

```rust
use sofab::{OStream, IStream, Visitor, Id, Unsigned, Signed, Result};

#[derive(Default)]
struct Telemetry { seq: u64, temp_c: i64, label: [u8; 16], label_len: usize }

impl Telemetry {
    const SEQ: Id = 1;
    const TEMP: Id = 2;
    const LABEL: Id = 3;

    fn encode(&self, buf: &mut [u8]) -> Result<usize> {
        let mut os = OStream::new(buf);
        os.write_unsigned(Self::SEQ, self.seq)?;
        os.write_signed(Self::TEMP, self.temp_c)?;
        os.write_blob(Self::LABEL, &self.label[..self.label_len])?;
        Ok(os.bytes_used())
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        IStream::new().feed(bytes, &mut msg)?;
        Ok(msg)
    }
}

impl Visitor for Telemetry {
    fn unsigned(&mut self, id: Id, v: Unsigned) { if id == Self::SEQ { self.seq = v; } }
    fn signed(&mut self, id: Id, v: Signed)     { if id == Self::TEMP { self.temp_c = v; } }
    fn blob(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        if id == Self::LABEL {
            for (i, &b) in chunk.iter().enumerate() {
                let pos = offset + i;
                if pos < self.label.len() { self.label[pos] = b; self.label_len = pos + 1; }
            }
        }
    }
}
```

The recording `Visitor` in [`tests/common/mod.rs`](tests/common/mod.rs) and the
per-type round-trips in [`tests/config_tests.rs`](tests/config_tests.rs) are
worked examples of this same encode/decode-into-fixed-storage pattern.

## API summary

### Encoding

[`OStream`] writes Sofab fields into a caller-owned `&mut [u8]` and **never
allocates**. Every writer returns `Result<()>`, returning `Error::BufferFull`
when the buffer fills and no [`Flush`] sink is attached (with a sink, it drains
and resumes instead). The surface is a set of typed writers:

- **scalars** — `write_unsigned`, `write_signed` (ZigZag), `write_boolean`;
- **fixed-length** (needs `fixlen`) — `write_fp32`, `write_fp64` (`fp64`),
  `write_str` (UTF-8, no NUL on the wire), `write_blob`, and the low-level
  `write_fixlen(id, &[u8], FixlenType)`;
- **arrays** (needs `array`) — `write_array_unsigned::<T>` / `write_array_signed::<T>`
  where the element width is fixed by `T` at compile time (`u8`/`u16`/`u32`, plus
  `u64`/`i64` with `value64`), and `write_array_fp32` / `write_array_fp64`;
- **nested sequences** (needs `sequence`) — `write_sequence_begin` /
  `write_sequence_end`, depth-checked against [`MAX_DEPTH`] (255).

Because the array element width is a compile-time type parameter, the C API's
"invalid element size" runtime error is simply impossible here.

### Decoding

[`IStream`] is a byte-at-a-time **push** decoder. You `feed` it arbitrarily small
chunks and it pushes each recovered value to your [`Visitor`]; it suspends and
resumes at any byte boundary, keeping all parse state inside the fixed-size
`IStream` struct, and **never allocates**. There is no read-scalar / read-string
call and no explicit skip or descend step — decoding is driven entirely by
`feed`. A `Visitor` method left at its default empty body **skips** that field at
zero cost. String/blob payloads arrive as one or more `(total, offset, chunk)`
callbacks; an empty string/blob is reported once with `total == 0`. Array
elements arrive after an `array_begin(id, kind, count)` via the scalar/float
callbacks carrying the same `id`.

### Memory handling

This is the defining property of the `no_std` port: **all storage is supplied by
the caller and nothing is ever boxed** — in both directions.

| Concern | Encoder ([`OStream`]) | Decoder ([`IStream`] + [`Visitor`]) |
|---------|-----------------------|-------------------------------------|
| Output / input buffer | the caller's `&mut [u8]`, borrowed for the stream's lifetime | the caller's `&[u8]`, borrowed only for the `feed` call |
| Message object | you build fields imperatively; no message struct is required | your `Visitor` — a caller struct with fixed-capacity fields |
| Allocation | none, ever | none, ever (state lives in the fixed `IStream` struct) |
| When data moves | each `write_*` copies into the buffer immediately | values are delivered **by value** into the callback the instant they decode |
| String / blob | copied from your `&str`/`&[u8]` into the buffer as written | delivered as a borrowed `chunk: &[u8]` that **points into the bytes you fed** — valid only for that callback; copy out anything you must keep |
| Overflow | buffer full → `Error::BufferFull`, or drained via the [`Flush`] sink | the decoder imposes no capacity; **your `Visitor` decides** where data lands and how to handle overflow (fixed array, truncate, or your own error) |
| Internal scratch | none beyond the output buffer | one 8-byte accumulator, only to reassemble an `fp32`/`fp64` split across `feed` boundaries |

Because the decoder pushes values *by value*, destinations need not be
address-stable and an unhandled field costs nothing. The only caller obligation
is to copy a string/blob `chunk` out of the transient `feed` input before the
callback returns if the bytes must outlive it.

Contrast with the `std` [`corelib-rs`](https://github.com/sofa-buffers/corelib-rs):
it allocates freely — strings/blobs land in owned `String`/`Vec<u8>`, arrays in
`Vec<…>`, and a one-shot `decode()` builds an owned result — trading a heap for
freedom from buffer management. See
[Choosing between the two Rust corelibs](#choosing-between-the-two-rust-corelibs).

## Feature flags

Every capability is **on by default** (mirroring the C library's full build).
The features positively *enable* wire types; turn them **off** (via
`default-features = false`, then re-enable what you need) to mirror the C
`SOFAB_DISABLE_*` switches and shrink the binary on tiny targets.

| Feature | Default | Enables |
|---------|:------:|---------|
| `fixlen` | ✅ | fp32, fp64, string, blob (`FIXLEN` / `FIXLENARRAY`) |
| `array` | ✅ | array fields (`VARINTARRAY_*`, `FIXLENARRAY`) |
| `sequence` | ✅ | nested sequences (`SEQUENCE_START` / `END`) |
| `fp64` | ✅ | 64-bit floats (implies `fixlen`) |
| `value64` | ✅ | 64-bit scalar value type (`u64`/`i64`); disable for 32-bit (`u32`/`i32`) |

```toml
# Smallest build: integers only, 32-bit values. The crate is still `sofab`.
sofa-buffers-corelib-no-std = { version = "0.1", default-features = false }
```

The wire-type flags are **additive**, but `value64` **controls a public type**
(`sofab::Unsigned`/`Signed`) and is therefore *not* additive: disabling it
narrows the scalar type to `u32`/`i32`, removing all double-width arithmetic and
the 64-bit libgcc/compiler helpers it drags in on a 32-bit MCU (the single
largest footprint item), at the cost that values above `2³²−1` can no longer be
represented or decoded (the decoder rejects an over-wide varint with
`Error::InvalidMsg`, mirroring a 32-bit `sofab_value_t` build of the C
reference).

### Verifying the build configuration

Because the wire types are compile-time switches, assert the ones your
application depends on with the [`require!`] macro — a missing capability fails
the **build**, not a device in the field:

```rust
// Compile error unless this `sofab` was built with fp64 + array support and 64-bit values.
sofab::require!(fp64, array, value64);
```

Accepted capabilities: `fixlen`, `array`, `sequence`, `fp64`, `value32`,
`value64`. The same information is available as plain constants in
[`sofab::config`] (`FIXLEN`, `ARRAY`, `SEQUENCE`, `FP64`, `VALUE_BITS`) for your
own `const` assertions or logging.

[`require!`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/macro.require.html
[`sofab::config`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/config/index.html

## Build & test

```bash
cargo build --all-features       # build with every feature enabled
cargo build                      # default features
cargo test --all-features        # unit + integration + doctests
cargo test                       # tests with default features
```

Prove the crate is genuinely `no_std` / heap-free by building the library for a
bare-metal target with no host `std`:

```bash
rustup target add thumbv7em-none-eabihf
cargo build --lib --all-features --target thumbv7em-none-eabihf
```

Tests live in `tests/` as separate integration files: `vectors_tests.rs`
(replays the shared `assets/test_vectors.json` — encode, chunked encode through
1/3/7-byte flush buffers, decode, chunked decode, and auto-skip; it is
`requires`-aware, so it runs under any feature subset), `ostream_tests.rs`,
`istream_tests.rs`, `roundtrip_tests.rs`, `api_tests.rs` (offset reserve, buffer
swap, large chunked streaming, API version), and `config_tests.rs`
(`#[cfg]`-gated per-configuration smoke tests). Line coverage is ~93%
(`cargo llvm-cov --all-features`); CI publishes the live number to the coverage
badge above.

To exercise the whole **feature powerset** — every on/off combination of
`fixlen` / `array` / `sequence` / `fp64` / `value64` — use
[`cargo-hack`](https://github.com/taiki-e/cargo-hack):

```bash
cargo hack --feature-powerset --no-dev-deps clippy --lib -- -D warnings  # compile + lint each config
cargo hack --feature-powerset test --test config_tests                   # run each config's smoke tests
```

All of the above are exactly the steps run in CI (see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## Benchmarks

Two tools mirror the C/C++ benchmark suite and run the **same** reference
workloads (a 1000-element integer array and a typical composite message), so
results are comparable across language ports:

```bash
cargo bench --bench perf    # per-op cost: HW cycles/op (x86 TSC / AArch64 counter) + MB/s
cargo bench --bench bench   # practical throughput in MB/s (MB = 1,000,000 bytes)
```

### Footprint

`tools/footprint.sh` measures the library `.text` size by linking a `no_std`
staticlib that exercises the full encode + decode API with the shipping release
profile (`opt-level="z"`, fat LTO, `panic="abort"`) and `--gc-sections`, then
reading the linked ELF with `llvm-size`. CI runs it on every push (the single
source of truth for these numbers):

```bash
tools/footprint.sh                             # Cortex-M0  (thumbv6m-none-eabi, default)
tools/footprint.sh thumbv7em-none-eabihf       # Cortex-M4F
tools/footprint.sh riscv32imc-unknown-none-elf # RISC-V 32 (RV32IMC)
```

| Configuration | Cortex-M0 `.text` | Cortex-M4F `.text` | RISC-V 32 `.text` |
|---------------|------------------:|-------------------:|------------------:|
| **MIN** — integers only, 32-bit (`default-features = false`) | **724 B** | **740 B** | **1 140 B** |
| integers only, 64-bit (`value64`) | 902 B | 936 B | 1 374 B |
| `+ sequence` (64-bit) | 1 002 B | 1 028 B | 1 522 B |
| `+ array` (64-bit) | 1 258 B | 1 238 B | 1 820 B |
| `+ fixlen` (fp32 / str / blob, 64-bit) | 1 501 B | 1 587 B | 2 109 B |
| all wire types, 32-bit | 1 893 B | 1 921 B | 3 061 B |
| **MAX** — all wire types, 64-bit (default) | **2 353 B** | **2 325 B** | **3 373 B** |

The codec spans **≈0.7 KiB** (integer-only, 32-bit) to **≈2.3 KiB** (every wire
type, 64-bit) of flash on Cortex-M0; disabling `value64` removes ~20% of the code
by deleting the 64-bit shift/`memclr` helpers (`__aeabi_llsl`, `__aeabi_memclr8`)
and halving every varint operation. The denser Thumb-2 encoding keeps the
Cortex-M builds smaller than RISC-V.

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust cores with the same wire format and the same
encoder/decoder API, tuned for opposite ends of the spectrum:

- **`corelib-rs-no-std`** (this crate) — `#![no_std]`, no allocator, fixed
  caller buffers, size-optimized profile. For **microcontrollers and
  footprint-constrained firmware**, where every KB of flash matters and there is
  no heap. In the multi-language benchmark arena it runs at roughly **1.13×
  micropb** throughput while fitting a bare-metal Cortex-M image of about
  **6.0 KB flash versus micropb's ~8.5 KB** (approximate, best-of-5; comparable
  only within the embedded-Rust group).
- **[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs)** — the `std`
  port, `opt-level = 3`, allocates freely (owned `String`/`Vec`, one-shot
  `decode()`). For **servers and desktops** that want maximum throughput and
  ergonomic ownership. In the arena it runs at roughly **1.4× prost** throughput.

| | `corelib-rs-no-std` (this crate) | `corelib-rs` (`std`) |
|---|---|---|
| Target | microcontrollers → servers | desktop / server |
| `std` / allocator | neither (`#![no_std]`, no `alloc`) | requires `std` |
| Buffers | caller-owned fixed capacity | library-allocated (`String`/`Vec`) |
| Decode model | push to a `Visitor`, zero-copy `chunk` views | owning one-shot `decode()` |
| Release profile | `opt-level = "z"`, LTO, `panic = "abort"` | `opt-level = 3`, LTO |
| Optimized for | small `.text` + zero heap | raw throughput |
| Arena result | ~1.13× micropb throughput; ~6.0 KB Cortex-M flash | ~1.4× prost throughput |

Both crates run the **identical** `perf` and `bench` tools, so a head-to-head is
just a matter of building each the way it ships — **size-optimized vs
speed-optimized, by design**. On one 6-core x86-64 host (median of 15 runs;
reproduce with the commands above) the size-tuned `no_std` build trails the
speed-tuned `std` build, and the gap widens with payload size:

| Workload | `no_std` MB/s | `std` MB/s | `std` faster |
| --- | ---: | ---: | ---: |
| serialize — typical message (170 B)   |  98.3 | 149.5 | 1.5× |
| deserialize — typical message (170 B) |  84.3 | 132.2 | 1.6× |
| encode — `u64` array ×1000 (9,491 B)  | 290.6 | 670.7 | 2.3× |
| decode — `u64` array ×1000 (9,491 B)  | 148.7 | 825.1 | 5.5× |

That is the deliberate trade-off: pick this crate for embedded and footprint —
where the `std` crate cannot build at all — and pick `corelib-rs` for servers
and throughput.
