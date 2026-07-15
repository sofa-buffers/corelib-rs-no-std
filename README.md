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
SofaBuffers (*Sofab*) serialization format. It runs on any platform, from tiny
microcontrollers to servers: `#![forbid(unsafe_code)]`, allocates nothing, and
keeps every byte of state in caller-provided buffers and structs — so it links
into firmware where an allocator (and the `std`-based
[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs)) cannot go.

### Requirements

Rust **1.70+** (MSRV), edition 2021, stable. Builds on any target, including
bare-metal `thumbv6m` / `thumbv7em` / `riscv32imc`.

### Dependencies

None at runtime — only `core` (no `alloc`). `libc` and `serde_json` are
`dev-dependencies` for benchmarks and the test suite.

### Packaging

The crates.io package is `sofa-buffers-corelib-no-std`; the compiled crate you
`use` is `sofab`.

```bash
cargo add sofa-buffers-corelib-no-std
```

## Why this design

| Goal | How |
|------|-----|
| No allocator | All state lives in caller buffers/structs; nothing is boxed. |
| No `unsafe` | `#![forbid(unsafe_code)]`; endianness via `to_le_bytes`/`from_le_bytes`. |
| Streaming **out** | [`OStream`] writes a small caller buffer and calls a [`Flush`] sink when it fills. |
| Streaming **in** | [`IStream`] is a byte-at-a-time state machine; large payloads arrive in pieces. |
| Reserve-offset | `OStream::with_offset` leaves room for a lower-layer header (saves a copy). |
| Small footprint | Cargo features drop whole code paths; `opt-level="z"`, LTO, `panic="abort"`. |

## Usage

The codec has four use cases — serialize a message that fits in one buffer,
serialize one too large for the buffer (streamed out in chunks), deserialize a
whole message, and deserialize one arriving in chunks — plus the generated-code
path that wraps them. Everything runs allocation-free on caller-owned buffers.

### Serialize

`OStream::new` borrows a caller-owned, fixed-capacity buffer big enough for the
whole message; write fields, then read the byte count:

```rust
use sofab::OStream;

let mut buf = [0u8; 64];                 // caller-owned, fixed capacity
let used = {
    let mut os = OStream::new(&mut buf); // borrows buf for its lifetime
    os.write_unsigned(1, 42).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_str(3, "hi").unwrap();
    os.bytes_used()
};
let wire = &buf[..used];
```

### Serialize stream

Give `OStream` a **tiny** window and a `Flush` sink (any `FnMut(&[u8])`, or a
manual `impl Flush` on bare metal); when the window fills it drains to the sink, so
the produced message can be far larger than RAM:

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

### Deserialize

Decoding is **push-based**: implement `Visitor` and the decoder calls back the
methods for the field kinds you care about; any method left at its default (empty)
body transparently **skips** that field.

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

### Deserialize stream

`IStream` resumes at any byte boundary, so feed it arbitrarily small chunks as they
arrive off the wire — from any source; string/blob payloads reach the visitor in
pieces, each `chunk` borrowing the bytes you fed.

Every `feed` returns the three-valued decode outcome of the bytes seen *so far*
(`MESSAGE_SPEC.md` §7): `Ok(())` means the stream is at a **field boundary**
(`COMPLETE`); `Err(Error::Incomplete)` means it stopped **mid-field** — a
first-class "feed me the next chunk" signal, *not* an error; `Err(Error::InvalidMsg)`
means the bytes are malformed regardless of what follows. There is no
`finish`/`finalize` step — end-of-input is the caller's own framing decision, so a
whole-message caller simply requires the final outcome to be `Ok(())`.

```rust
use sofab::{IStream, Visitor, Id, Error};

#[derive(Default)]
struct Len { total: usize }
impl Visitor for Len {
    fn blob(&mut self, _id: Id, total: usize, _offset: usize, chunk: &[u8]) {
        self.total = total;                   // `chunk` borrows the fed bytes
        let _ = chunk;                        // copy it out here if you need it later
    }
}

let mut sink = Len::default();
let mut is = IStream::new();
for piece in wire.chunks(4) {                 // one packet at a time, from any source
    match is.feed(piece, &mut sink) {
        Ok(()) | Err(Error::Incomplete) => {} // at a boundary, or mid-field: keep feeding
        Err(e) => panic!("malformed: {e:?}"), // INVALID: terminal
    }
}
```

### Code generator

The common real use is a schema compiled by **`sofabgen`** into typed structs
whose `encode` / `decode` methods drive this runtime into fixed caller storage.
This crate ships the *runtime*; generated code calls it exactly like this
hand-written stand-in:

```rust
use sofab::{OStream, IStream, Visitor, Id, Signed, Result};

// generated by: sofabgen --lang rust (no_std profile)
#[derive(Default)]
struct Point { x: i64, y: i64 }

impl Point {
    fn encode(&self, buf: &mut [u8]) -> Result<usize> {
        let mut os = OStream::new(buf);
        os.write_signed(1, self.x)?;
        os.write_signed(2, self.y)?;
        Ok(os.bytes_used())
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut m = Self::default();
        IStream::new().feed(bytes, &mut m)?;
        Ok(m)
    }
}

impl Visitor for Point {
    fn signed(&mut self, id: Id, v: Signed) { match id { 1 => self.x = v, 2 => self.y = v, _ => {} } }
}

let mut buf = [0u8; 32];
let n = Point { x: 3, y: 4 }.encode(&mut buf).unwrap();
let got = Point::decode(&buf[..n]).unwrap();   // got.x == 3, got.y == 4
```

## Memory handling

The defining property of the `no_std` port: **all storage is caller-supplied and
nothing is ever boxed — no allocation in either direction.**

- **Encode ([`OStream`])** — writes into the caller's `&mut [u8]`, borrowed for
  the stream's lifetime; each `write_*` copies into it immediately. Buffer full
  → `Error::BufferFull`, or drained via the [`Flush`] sink.
- **Decode ([`IStream`] + [`Visitor`])** — reads the caller's `&[u8]`, borrowed
  only for the `feed` call; values are delivered **by value** the instant they
  decode (so destinations need not be address-stable). A string/blob
  `chunk: &[u8]` **borrows the bytes you fed** and is valid only for that
  callback — copy out anything you must keep. Your `Visitor` decides where data
  lands and how to handle overflow. State lives in the fixed `IStream` struct
  (one 8-byte fp accumulator), never allocating.

| | Encoder ([`OStream`]) | Decoder ([`IStream`] + [`Visitor`]) |
|---|---|---|
| Buffer | caller's `&mut [u8]`, borrowed for the stream's lifetime | caller's `&[u8]`, borrowed only for the `feed` call |
| Allocation | none, ever | none, ever (state in the fixed `IStream` struct) |

## Feature flags

Every capability is **on by default**. The features positively *enable* wire
types; turn them **off** (`default-features = false`, then re-enable what you
need) to shrink the binary on tiny targets.

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


> **`value64` — change only if you know what you are doing.**
> It shrinks 64-bit varint math (smaller/faster on 32-bit MCUs) but has wire-
> and API-level side effects:
> - **Wire compatibility:** the format is width-agnostic, so messages whose values
>   all fit in 32 bits stay byte-identical and interoperable. A value beyond the
>   32-bit range from a 64-bit peer is **rejected** as malformed (`Error::InvalidMsg`) —
>   never silently truncated.
> - **ABI:** the value types appear in public signatures, so 32-bit and 64-bit
>   builds are **not** ABI-compatible — don't mix them.
> - **Field ids:** the effective field-id range shrinks, since the field header is a
>   varint of `(id << 3) | type`.
> - **Conformance:** the shipped test vectors include 64-bit values and won't
>   decode in this mode.

(Array element widths are compile-time type parameters, so an invalid element
size is unrepresentable.)

### Verifying the build configuration

Because the wire types are compile-time switches, assert the ones your
application depends on with the [`require!`] macro — a missing capability fails
the **build**, not a device in the field:

```rust
// Compile error unless this `sofab` was built with fp64 + array support and 64-bit values.
sofab::require!(fp64, array, value64);
```

Accepted capabilities: `fixlen`, `array`, `sequence`, `fp64`, `value32`,
`value64`. The same information is available as constants in [`sofab::config`]
(`FIXLEN`, `ARRAY`, `SEQUENCE`, `FP64`, `VALUE_BITS`) for `const` assertions or
logging.

[`require!`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/macro.require.html
[`sofab::config`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/config/index.html

## Build & test

```bash
cargo build --all-features       # every feature enabled
cargo test --all-features        # unit + integration + doctests
```

Prove the crate is genuinely `no_std` / heap-free by building for a bare-metal
target with no host `std`:

```bash
rustup target add thumbv7em-none-eabihf
cargo build --lib --all-features --target thumbv7em-none-eabihf
```

Integration tests live in `tests/`: `vectors_tests.rs` (replays the shared
`assets/test_vectors.json`, feature-aware), `ostream_tests.rs`,
`istream_tests.rs`, `roundtrip_tests.rs`, `api_tests.rs`, and `config_tests.rs`.
Line coverage is ~93% (`cargo llvm-cov --all-features`). To exercise the whole
feature powerset, use [`cargo-hack`](https://github.com/taiki-e/cargo-hack):

```bash
cargo hack --feature-powerset --no-dev-deps clippy --lib -- -D warnings
cargo hack --feature-powerset test --test config_tests
```

All of the above are the exact steps run in CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## Benchmarks

Two tools run the **same** reference workloads (a 1000-element integer array and
a typical composite message), so results are comparable across language ports:

```bash
cargo bench --bench perf    # per-op cost: HW cycles/op + MB/s
cargo bench --bench bench   # throughput in MB/s (MB = 1,000,000 bytes)
```

### Footprint

`tools/footprint.sh` measures the library's **flash** and **RAM** footprint by
linking a `no_std` staticlib that exercises the full encode + decode API with the
release profile (`opt-level="z"`, fat LTO, `panic="abort"`) and `--gc-sections`.
CI runs it on every push:

```bash
tools/footprint.sh                             # Cortex-M0  (thumbv6m-none-eabi, default)
tools/footprint.sh thumbv7em-none-eabihf       # Cortex-M4F
tools/footprint.sh riscv32imc-unknown-none-elf # RISC-V 32 (RV32IMC)
```

**Flash** (`.text + .data`). The library defines no statics, so `.data`/`.bss`
are zero and flash equals `.text`:

| Configuration | Cortex-M0 | Cortex-M4F | RISC-V 32 |
|---------------|----------:|-----------:|----------:|
| **MIN** — integers only, 32-bit (`default-features = false`) | **566 B** | **562 B** | **616 B** |
| integers only, 64-bit (`value64`) | 732 B | 742 B | 814 B |
| `+ sequence` (64-bit) | 876 B | 858 B | 946 B |
| `+ array` (64-bit) | 1 112 B | 1 056 B | 1 216 B |
| `+ fixlen` (fp32 / str / blob, 64-bit) | 1 247 B | 1 321 B | 1 311 B |
| all wire types, 32-bit | 1 779 B | 1 871 B | 2 157 B |
| **MAX** — all wire types, 64-bit (default) | **2 195 B** | **2 231 B** | **2 529 B** |

The codec spans **≈0.55 KiB** (integer-only, 32-bit) to **≈2.1 KiB** (every wire
type, 64-bit) of flash on Cortex-M0; disabling `value64` removes ~20% of the code
by deleting the 64-bit shift/`memclr` helpers and halving every varint
operation. The decoder carries no panic paths (all bounds are proven in-bounds),
so the whole codec links without `core::panicking` — which is what keeps the
RISC-V builds, lacking Thumb-2's density, close behind Cortex-M.

**RAM.** There is no heap and no static RAM — the only runtime state is the
caller-provided `IStream` (decoder) and `OStream` (encoder), usually stack
allocated. Sizes are identical across these 32-bit targets:

| Configuration | `IStream` | `OStream` | total |
|---------------|----------:|----------:|------:|
| **MIN** — integers only, 32-bit | 16 B | 16 B | **32 B** |
| integers only, 64-bit | 24 B | 16 B | 40 B |
| `+ sequence` (64-bit) | 32 B | 20 B | 52 B |
| `+ array` (64-bit) | 32 B | 16 B | 48 B |
| `+ fixlen` (64-bit) | 40 B | 16 B | 56 B |
| all wire types, 32-bit | 40 B | 20 B | 60 B |
| **MAX** — all wire types, 64-bit (default) | 48 B | 20 B | **68 B** |

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust cores with the same wire format and the same
encoder/decoder API, tuned for opposite ends of the spectrum:

- **`corelib-rs-no-std`** (this crate) — `#![no_std]`, no allocator, fixed
  caller buffers, size-optimized profile. For **microcontrollers and
  footprint-constrained firmware**. In the multi-language arena it runs at
  roughly **1.13× micropb** throughput while fitting a bare-metal Cortex-M image
  of about **6.0 KB flash versus micropb's ~8.5 KB**.
- **[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs)** — the `std`
  port, `opt-level = 3`, allocates freely (owned `String`/`Vec`, one-shot
  `decode()`). For **servers and desktops** wanting maximum throughput and
  ergonomic ownership; roughly **1.4× prost** throughput.

| | `corelib-rs-no-std` (this crate) | `corelib-rs` (`std`) |
|---|---|---|
| Target | microcontrollers → servers | desktop / server |
| `std` / allocator | neither (`#![no_std]`, no `alloc`) | requires `std` |
| Buffers | caller-owned fixed capacity | library-allocated (`String`/`Vec`) |
| Decode model | push to a `Visitor`, zero-copy `chunk` views | owning one-shot `decode()` |
| Release profile | `opt-level = "z"`, LTO, `panic = "abort"` | `opt-level = 3`, LTO |
| Optimized for | small `.text` + zero heap | raw throughput |
| Arena result | ~1.13× micropb throughput; ~6.0 KB Cortex-M flash | ~1.4× prost throughput |

Both crates run the **identical** `perf` and `bench` tools. On one 6-core x86-64
host (median of 15 runs) the size-tuned `no_std` build trails the speed-tuned
`std` build, and the gap widens with payload size:

| Workload | `no_std` MB/s | `std` MB/s | `std` faster |
| --- | ---: | ---: | ---: |
| serialize — typical message (170 B)   |  98.3 | 149.5 | 1.5× |
| deserialize — typical message (170 B) |  84.3 | 132.2 | 1.6× |
| encode — `u64` array ×1000 (9,491 B)  | 290.6 | 670.7 | 2.3× |
| decode — `u64` array ×1000 (9,491 B)  | 148.7 | 825.1 | 5.5× |

That is the deliberate trade-off: pick this crate for embedded and footprint —
where the `std` crate cannot build at all — and pick `corelib-rs` for servers
and throughput.
