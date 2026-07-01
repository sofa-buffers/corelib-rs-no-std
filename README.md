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
microcontrollers to desktops and servers.

**Minimum Rust version:** 1.70. **Install** (the crates.io package is
`sofa-buffers-corelib-no-std`; you still `use sofab::…` in code):

```bash
cargo add sofa-buffers-corelib-no-std
```

The wire format is specified, language-neutrally, in the
[SofaBuffers documentation](https://github.com/sofa-buffers/documentation). For
byte-for-byte interoperability across every language port, the test suite
replays the **shared** cross-language test vectors
([`assets/test_vectors.json`](assets/test_vectors.json), copied verbatim from
the documentation repository — the single source of truth) and asserts the
encoder's output and the decoder's recovered fields match for all of them.

This library implements SofaBuffers **API version 1** (exposed as
`sofab::API_VERSION`).

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

```rust
use sofab::{OStream, IStream, Visitor, Id, Unsigned, Signed};

// ---- encode (no heap, fixed buffer) ----
let mut buf = [0u8; 64];
let mut os = OStream::new(&mut buf);
os.write_unsigned(1, 42).unwrap();
os.write_signed(2, -7).unwrap();
os.write_str(3, "hi").unwrap();
let used = os.bytes_used();

// ---- decode (push to your Visitor) ----
#[derive(Default)]
struct My { a: u64, b: i64 }
impl Visitor for My {
    fn unsigned(&mut self, id: Id, v: Unsigned) { if id == 1 { self.a = v; } }
    fn signed(&mut self, id: Id, v: Signed)     { if id == 2 { self.b = v; } }
    // string(), blob(), fp32(), array_begin(), sequence_begin(), ... as needed
}
let mut sink = My::default();
let mut is = IStream::new();
is.feed(&buf[..used], &mut sink).unwrap();
```

### Streaming a message larger than the buffer

```rust
use sofab::OStream;
let mut scratch = [0u8; 16];                 // tiny buffer
let mut out = Vec::new();                     // or a UART/socket
let mut os = OStream::with_flush(&mut scratch, 0, |chunk: &[u8]| out.extend_from_slice(chunk));
for i in 0..1000u32 { os.write_unsigned(i, i as u64).unwrap(); }
os.flush();                                   // push the tail
```

## API summary

### Write operations — [`OStream`]

Writes Sofab fields into a caller-owned buffer; **never allocates**. Every writer
returns `Result<()>` and emits `Error::BufferFull` when the buffer is full and no
[`Flush`] sink is attached.

| Operation | Signature (`id: Id`, …) | Purpose |
|-----------|-------------------------|---------|
| `new` / `with_offset` / `with_flush` | `(&mut [u8])` / `(&mut [u8], offset)` / `(&mut [u8], offset, sink)` | construct over a caller buffer; reserve a header offset; attach a flush sink |
| `write_unsigned` / `write_signed` / `write_boolean` | `(id, Unsigned)` / `(id, Signed)` / `(id, bool)` | scalar integers (varint / zig-zag) and booleans |
| `write_fp32` / `write_fp64` | `(id, f32)` / `(id, f64)` | little-endian IEEE-754 floats (`fp64` feature for `f64`) |
| `write_str` / `write_blob` | `(id, &str)` / `(id, &[u8])` | UTF-8 text (no NUL on the wire) / raw bytes (`fixlen`) |
| `write_fixlen` | `(id, &[u8], FixlenType)` | raw fixed-length field with an explicit subtype (`fixlen`) |
| `write_array_unsigned` / `write_array_signed` | `(id, &[T])` | varint arrays; `T` is an integer-width element type (see below) (`array`) |
| `write_array_fp32` / `write_array_fp64` | `(id, &[f32])` / `(id, &[f64])` | float arrays with one shared element descriptor (`array` + `fixlen`/`fp64`) |
| `write_sequence_begin` / `write_sequence_end` | `(id)` / `()` | open / close a nested sequence (`sequence`) |
| `flush` / `buffer_set` / `bytes_used` | `()` / `(&mut [u8], offset)` / `()` | drain pending bytes to the sink; swap the output buffer mid-stream; bytes written so far |

### Read operations — [`IStream`] + [`Visitor`]

[`IStream`] is a byte-at-a-time **push** decoder: feed it arbitrarily small chunks
and it pushes each recovered value to your [`Visitor`]. It suspends and resumes at
any byte boundary, keeping all parse state inside the (fixed-size) `IStream`
struct — it **never allocates** and binds no caller destinations up front.

| Operation | Signature | Hands the caller |
|-----------|-----------|------------------|
| `IStream::new` | `() -> IStream` | a fresh decoder (also `Default`); `const fn` |
| `feed` | `(&[u8], &mut V) -> Result<()>` | drives decoding of one chunk; pushes fields to the visitor |
| `Visitor::unsigned` / `signed` | `(id: Id, value: Unsigned)` / `(id, Signed)` | a scalar integer **by value**, or one unsigned/signed array element |
| `Visitor::fp32` / `fp64` | `(id, f32)` / `(id, f64)` | a float **by value**, or one float array element (`fixlen` / `fp64`) |
| `Visitor::string` / `blob` | `(id, total: usize, offset: usize, chunk: &[u8])` | one **chunk** of a string/blob: `total` field length, `offset` of this chunk, and a `chunk` slice **borrowed from the fed input** (`fixlen`) |
| `Visitor::array_begin` | `(id, kind: ArrayKind, count: usize)` | the start of an array; the `count` elements then arrive through the scalar/float callbacks with the same `id` (`array`) |
| `Visitor::sequence_begin` / `sequence_end` | `(id)` / `()` | nested-sequence framing (`sequence`) |

There is **no read-scalar / read-string / skip method to call** and no
descend-into-sequence call: decoding is driven entirely by `feed`, and the
decoder *pushes* to the visitor. A `Visitor` method left at its default (empty)
implementation transparently **skips** that field — the equivalent of the C
decoder's auto-skip — at zero cost and with no buffer required. An empty
string/blob is reported once with `total == 0` and an empty `chunk`.

### Allowed types

The wire/template types accepted by the typed APIs:

| Category | Encoder | Decoder callback | Concrete types |
|----------|---------|------------------|----------------|
| Unsigned scalar | `write_unsigned` | `Visitor::unsigned` | `Unsigned` = `u64` (or `u32` with `value64` off) |
| Signed scalar | `write_signed` | `Visitor::signed` | `Signed` = `i64` (or `i32` with `value64` off) |
| Boolean | `write_boolean` | `Visitor::unsigned` (`0`/`1`) | `bool` |
| Float | `write_fp32` / `write_fp64` | `fp32` / `fp64` | `f32`, `f64` (`f64` needs `fp64`) |
| String / blob | `write_str` / `write_blob` | `string` / `blob` | `&str` / `&[u8]` |
| Unsigned array | `write_array_unsigned<T>` | `unsigned` per element | `T: UnsignedElem` — `u8`, `u16`, `u32` (and `u64` with `value64`) |
| Signed array | `write_array_signed<T>` | `signed` per element | `T: SignedElem` — `i8`, `i16`, `i32` (and `i64` with `value64`) |
| Float array | `write_array_fp32` / `write_array_fp64` | `fp32` / `fp64` per element | `[f32]` / `[f64]` |

The element width of an array is fixed by `T` at compile time (via the
[`UnsignedElem`] / [`SignedElem`] marker traits), so the C API's "invalid element
size" runtime error is impossible here. **Disallowed:** strings and blobs are not
valid as fixed-length-array elements — the encoder offers no such call and the
decoder rejects that wire shape with `Error::InvalidMsg`; and with `value64` off,
a varint that would exceed `2³²−1` is rejected as `Error::InvalidMsg`. A
**zero-count array is valid** (§4.7/§4.8): it encodes as `[ header ][ count = 0 ]`
— a fixlen array carries no element descriptor when empty — and decodes to a
single `array_begin(.., 0)`.

### Memory handling

This is the defining property of the `no_std` port: **all storage is supplied by
the caller and nothing is ever boxed.** Both directions are allocation-free.

| Concern | Encoder ([`OStream`]) | Decoder ([`IStream`] + [`Visitor`]) |
|---------|-----------------------|-------------------------------------|
| Who owns the buffer | the caller — a `&mut [u8]` you hand to `new` / `with_offset` / `with_flush` | the caller — the `&[u8]` you pass to `feed`, plus whatever your `Visitor` stores into |
| Allocation | none, ever | none, ever (state lives in the fixed `IStream` struct) |
| When data moves | each `write_*` copies into the buffer immediately | values are **copied by value** into the `Visitor` callback the instant they decode — there is **no lazy "bind a destination" step** as in the C decoder |
| String / blob payload | copied from your `&str`/`&[u8]` into the buffer as it is written | delivered as a borrowed `chunk: &[u8]` that **points into the bytes you fed**; valid only for the duration of the callback — copy out anything you need to keep |
| Array payload | read straight from your `&[T]` slice | streamed element-by-element through the scalar/float callbacks; no element buffer is held internally |
| Internal scratch | none beyond the output buffer | one 8-byte accumulator to reassemble a single `fp32`/`fp64` split across `feed` boundaries — nothing else is buffered |
| Full / overflow | buffer full → `Error::BufferFull`; or, with a [`Flush`] sink, the bytes are drained and writing resumes at the buffer start | the decoder imposes no capacity; **your `Visitor` decides where string/blob/array data lands and how to handle overflow** (e.g. copy into a fixed array, truncate, or raise your own error) |
| Mid-stream buffer swap | `buffer_set` installs a fresh buffer (typically from inside a flush sink); `with_offset` reserves header room up front | n/a — chunks are consumed in place |

Because the decoder pushes values *by value* (no up-front binding), destinations
do **not** need to be address-stable, and a field you don't handle costs nothing.
The only caller obligation is to copy string/blob `chunk` slices out of the
transient `feed` input before the callback returns if the bytes must outlive it.

Contrast with [`corelib-rs`](https://github.com/sofa-buffers/corelib-rs) (the
`std` port): it allocates freely — recovered strings/blobs land in owned `String`
/ `Vec<u8>`, arrays in `Vec<…>`, and a one-shot `decode()` builds an owned result
— so the caller is freed from buffer management at the cost of a heap.

## Differences from `corelib-rs` (the high-speed `std` port)

Same wire format and same encoder API as
[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs); only the items below
differ.

| Only in this crate (`no_std`) | Only in `corelib-rs` (`std`) |
|-------------------------------|------------------------------|
| `config` module + [`require!`] macro | `decode()` one-shot helper |
| Cargo feature flags (`fixlen`/`array`/`sequence`/`fp64`/`value64`) | `IStream::finish()` (detect a truncated message) |
| 32-bit value width option (`value64` off → `u32`/`i32`) | `IStream::reset()` (reuse without reallocating) |
| — | `Error: Display + std::error::Error` |

## Feature flags

Every capability is **on by default** (mirroring the C library's full build);
mirror the C `SOFAB_DISABLE_*` switches by turning features *off* to shrink the
binary on tiny targets, with `default-features = false`.

| Feature | Default | Enables |
|---------|:------:|---------|
| `fixlen` | ✅ | fp32, fp64, string, blob (`FIXLEN`/`FIXLENARRAY`) |
| `array` | ✅ | array fields (`VARINTARRAY_*`, `FIXLENARRAY`) |
| `sequence` | ✅ | nested sequences (`SEQUENCE_START`/`END`) |
| `fp64` | ✅ | 64-bit floats (implies `fixlen`) |
| `value64` | ✅ | 64-bit scalar value type (`u64`/`i64`); disable for 32-bit (`u32`/`i32`) |

Example minimal build (integers only, 32-bit values — smallest possible). With
`default-features = false` and nothing re-enabled, every capability (including
`value64`) is off:

```toml
# crates.io package name; the imported crate is still `sofab`
sofa-buffers-corelib-no-std = { version = "0.1", default-features = false }
```

> **Note on value width:** like the C default configuration, the scalar value
> type is 64-bit (`u64`/`i64`) — the default-on `value64` feature. On a 32-bit
> target the 64-bit type pulls in libgcc/compiler helpers (e.g. `__aeabi_llsl`,
> 8-byte `memclr`) and widens every varint operation — the single largest
> footprint item. *Disabling* `value64` narrows the value type to `u32`/`i32`,
> deleting that double-width arithmetic and the helpers it drags in. The
> trade-off is that values above `2³²−1` can no longer be represented or decoded
> (the decoder rejects an over-wide varint with `Error::InvalidMsg`, mirroring a
> 32-bit `sofab_value_t` build of the C reference). Unlike the wire-type flags,
> the value width *controls a public type* and so is **not additive** —
> application code that relies on a specific width should guard it with
> `sofab::require!(value64)` / `require!(value32)` (see *Verifying the build
> configuration* below).

### Verifying the build configuration

The wire types are compile-time switches, so a binary built with the wrong
feature set would silently lack a field type. To harden an application against
that (the Rust equivalent of a C `#ifdef` / `static_assert` guard), assert the
capabilities you depend on with the [`require!`] macro — a missing one fails the
**build**, not a device in the field:

```rust
// Stops the build unless this `sofab` is compiled with fp64 + array support
// and the 64-bit value width.
sofab::require!(fp64, array, value64);
```

Accepted capabilities: `fixlen`, `array`, `sequence`, `fp64`, `value32`,
`value64`. The same information is available as plain constants in
[`sofab::config`] (`FIXLEN`, `ARRAY`, `SEQUENCE`, `FP64`, `VALUE_BITS`) for use
in your own `const` assertions or logging.

[`require!`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/macro.require.html
[`sofab::config`]: https://sofa-buffers.github.io/corelib-rs-no-std/sofab/config/index.html

## Footprint

`.text` of the library, measured by linking a `no_std` staticlib that exercises
the encode + decode API with the size-optimized release profile
(`opt-level="z"`, fat LTO, `panic="abort"`) and `--gc-sections`. Columns are
three representative bare-metal targets:

| Configuration | Cortex-M0 `.text` | Cortex-M4F `.text` | RISC-V 32 `.text` |
|---------------|------------------:|-------------------:|------------------:|
| **MIN** — integers only, 32-bit (`default-features = false`) | **724 B** | **740 B** | **1 140 B** |
| integers only, 64-bit (`value64`) | 902 B | 936 B | 1 374 B |
| `+ sequence` (64-bit) | 982 B | 1 008 B | 1 480 B |
| `+ array` (64-bit) | 1 250 B | 1 238 B | 1 820 B |
| `+ fixlen` (fp32 / str / blob, 64-bit) | 1 501 B | 1 587 B | 2 109 B |
| all wire types, 32-bit (`fixlen,array,sequence,fp64`) | 1 797 B | 1 825 B | 2 977 B |
| **MAX** — all wire types, 64-bit (default / `--all-features`) | **2 229 B** | **2 245 B** | **3 321 B** |

Cortex-M0/M4F are `thumbv6m-none-eabi` / `thumbv7em-none-eabihf`; RISC-V 32 is
`riscv32imc-unknown-none-elf` — the denser Thumb-2 encoding keeps the Cortex-M
builds smaller. On Cortex-M0 the codec spans **≈0.7 KiB** (integer-only, 32-bit
values) to **≈2.2 KiB** (every wire type, 64-bit values) of flash; disabling
`value64` removes ~20 % of the code — chiefly by deleting the 64-bit
shift/`memclr` helpers (`__aeabi_llsl`, `__aeabi_memclr8`) and halving the width
of every varint operation.

Reproduce these numbers (and break them down per symbol) with:

```bash
tools/footprint.sh                            # Cortex-M0 (thumbv6m-none-eabi, default)
tools/footprint.sh thumbv7em-none-eabihf      # Cortex-M4F
tools/footprint.sh riscv32imc-unknown-none-elf # RISC-V 32 (RV32IMC)
```

## Build & test

```bash
cargo build --all-features       # build with every feature enabled
cargo build                      # build with default features
cargo test --all-features        # unit + integration + doctests
cargo test                       # tests with default features
```

To prove the crate is genuinely `no_std` / heap-free, build the library for a
bare-metal target with no host `std`:

```bash
rustup target add thumbv7em-none-eabihf
cargo build --lib --all-features --target thumbv7em-none-eabihf
```

These are exactly the steps run in CI (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

```bash
./coverage.sh                    # llvm-cov: terminal summary + HTML + lcov.info
```

Tests live in `tests/` as separate integration files:

- `vectors_tests.rs` — replays the shared `assets/test_vectors.json` (encode,
  chunked-encode through 1/3/7-byte flush buffers, decode, chunked-decode, and
  `skip_ids` auto-skip). It is `requires`-aware, so it runs under any feature
  subset and skips vectors a reduced build can't represent (`int64` → `value64`)
- `ostream_tests.rs` — encoder, byte-exact vs. reference vectors
- `istream_tests.rs` — decoder over the same vectors + malformed-input errors
- `roundtrip_tests.rs` — encode→decode value preservation
- `api_tests.rs` — offset reserve, buffer swap, large chunked streaming, API version
- `config_tests.rs` — per-configuration encode→decode smoke tests; `#[cfg]`-gated
  so they build and run under **any** feature subset (the conformance suites
  above need every wire type and only build with all features on)
- `tests/common/mod.rs` — shared recording `Visitor`

Current coverage: **~93% lines** (`cargo llvm-cov --all-features`).

### Testing every feature combination

The conformance suites run with all features. To check the whole
**feature powerset** — every on/off combination of `fixlen` / `array` /
`sequence` / `fp64` / `value64`, including the 32-bit value width — use
[`cargo-hack`](https://github.com/taiki-e/cargo-hack):

```bash
cargo install cargo-hack
cargo hack --feature-powerset --no-dev-deps clippy --lib -- -D warnings  # compile + lint each config
cargo hack --feature-powerset test --test config_tests                   # run each config's smoke tests
```

CI runs both of these (see the `features` job in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

Coverage prerequisites (one-time):

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
```

## Benchmarks

Two tools mirror the C/C++ benchmark suite and run the **same** reference
workloads (a 1000-element integer array and a typical composite message), so
results are comparable across language ports.

`perf` — CPU-speed-independent per-operation cost: hardware cycles/op (x86 TSC /
AArch64 counter) plus CPU ns/op and throughput, measured over a ~1 s CPU-time
loop:

```bash
cargo bench --bench perf
```

`bench` — practical throughput in **MB/s** (MB = 1,000,000 bytes), against
process CPU time, for encode and decode of each workload:

```bash
cargo bench --bench bench
```

### `no_std` vs `std`: how the two Rust ports compare

`corelib-rs-no-std` (this crate, `#![no_std]`) and the
[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs) `std` build implement
the **same SofaBuffers API** and run the **identical** `perf` and `bench` tools
— so the numbers reflect the two implementations, not the benchmark. Crucially,
each is built **the way it is meant to ship**, which is the comparison that
actually matters:

- **`corelib-rs-no-std` — full features, tuned for a small `.text`:**
  `opt-level = "z"`, LTO, `codegen-units = 1` (this crate's release profile).
- **`corelib-rs` — tuned for raw speed:** `opt-level = 3`, fat LTO,
  `codegen-units = 1`.

So this is a **size-optimized vs speed-optimized** comparison, by design.
Median of 15 runs on a single 6-core x86-64 VM (median is robust to the VM's
run-to-run jitter); `cycles/op` lower is better, MB/s higher is better.

| Workload | `no_std` cycles/op | `std` cycles/op | `no_std` MB/s | `std` MB/s | `std` faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| serialize — typical message (170 B)   |   4,835 |  3,178 |  98.3 | 149.5 | 1.5× |
| deserialize — typical message (170 B) |   5,636 |  3,600 |  84.3 | 132.2 | 1.6× |
| encode — `u64` array ×1000 (9,491 B)  |  91,272 | 39,614 | 290.6 | 670.7 | 2.3× |
| decode — `u64` array ×1000 (9,491 B)  | 178,368 | 32,152 | 148.7 | 825.1 | 5.5× |

**In plain terms:** built for a small footprint (`opt-level = "z"`), this crate
is slower than the speed-tuned `std` build on every workload, and the gap
**grows with payload size** — about 1.5× on a small typical message, 2.3×
encoding a 1000-element `u64` array, and up to **5.5×** decoding one. That is the
deliberate trade-off: `corelib-rs-no-std` gives up throughput for a tiny,
allocator-free binary that runs on microcontrollers — where the `std` crate
cannot build at all — while [`corelib-rs`](https://github.com/sofa-buffers/corelib-rs)
spends code size to go fast. Pick this crate for embedded and footprint; pick
`std` for servers and throughput.
