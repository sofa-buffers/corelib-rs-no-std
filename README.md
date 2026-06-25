<p align="center"><img src="assets/sofabuffers_logo.png" alt="SofaBuffers Logo" height="140"></p>

<h1 align="center">SofaBuffers</h1>

<p align="center">
<b>Structured Objects For Anyone</b><br>
<i>... so optimized, feels amazing.</i>
</p>

<p align="center"><a href="https://github.com/sofa-buffers">Would you like to know more?</a></p>

## SofaBuffers Rust library

[GitHub repository](https://github.com/sofa-buffers/corelib-rs)

[![CI](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fsofa-buffers%2Fcorelib-rs%2Fbadges%2Fcoverage.json)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-1f7feb)](https://sofa-buffers.github.io/corelib-rs/)

A `#![no_std]`, **heap-free**, **streaming** Rust implementation of the
SofaBuffers (*Sofab*) serialization format. It is a port of the C `corelib`
(`istream.c` / `ostream.c`) and runs on any platform, from tiny
microcontrollers to desktops and servers.

**Minimum Rust version:** 1.70. **Install:**

```bash
cargo add sofab
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

### Source documentation

[Documentation](https://sofa-buffers.github.io/corelib-rs/)

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

**Encoder — [`OStream`]** (writes into a caller buffer; never allocates):

| Operation | Purpose |
|-----------|---------|
| `new` / `with_offset` / `with_flush` | construct over a buffer; reserve a header offset; attach a flush sink |
| `write_unsigned` / `write_signed` / `write_boolean` | scalar integers (varint / zig-zag) and booleans |
| `write_fp32` / `write_fp64` / `write_str` / `write_blob` / `write_fixlen` | fixed-length values (LE floats, UTF-8 text, raw bytes) |
| `write_array_unsigned` / `write_array_signed` / `write_array_fp32` / `write_array_fp64` | arrays with a single shared descriptor |
| `write_sequence_begin` / `write_sequence_end` | open / close a nested sequence |
| `flush` / `buffer_set` / `bytes_used` | drain pending bytes; swap the output buffer mid-stream; bytes written |

**Decoder — [`IStream`] + [`Visitor`]** (push-feed; suspends/resumes at any byte boundary):

| Operation | Purpose |
|-----------|---------|
| `IStream::new` | construct a fresh decoder |
| `feed(bytes, visitor)` | feed an arbitrarily small chunk; decoded fields are pushed to the visitor |
| `Visitor::unsigned` / `signed` / `fp32` / `fp64` | scalar fields and array elements |
| `Visitor::string` / `blob` | fixed-length payloads, delivered in chunks (`total` / `offset` / `chunk`) |
| `Visitor::array_begin` | start of an array (`kind`, `count`); elements follow via the scalar/float callbacks |
| `Visitor::sequence_begin` / `sequence_end` | nested-sequence framing |

A `Visitor` method left at its default (empty) implementation transparently skips
that field — the equivalent of the C decoder's auto-skip.

## Feature flags

Mirror the C `SOFAB_DISABLE_*` switches, expressed positively. Turn features off
to shrink the binary on tiny targets.

| Feature | Default | Enables |
|---------|:------:|---------|
| `fixlen` | ✅ | fp32, fp64, string, blob (`FIXLEN`/`FIXLENARRAY`) |
| `array` | ✅ | array fields (`VARINTARRAY_*`, `FIXLENARRAY`) |
| `sequence` | ✅ | nested sequences (`SEQUENCE_START`/`END`) |
| `fp64` | ✅ | 64-bit floats (implies `fixlen`) |

Example minimal build (integers only):

```toml
sofab = { version = "0.1", default-features = false }
```

> **Note on value width:** like the C default configuration, the scalar value
> type is 64-bit (`u64`/`i64`). On a 32-bit target this pulls in libgcc/compiler
> 64-bit helpers — the single largest footprint item (see the SofaBuffers
> [documentation](https://github.com/sofa-buffers/documentation) footprint
> notes). A 32-bit value mode is a planned feature.

## Layering vs. the C library

| C file | Rust module | Status |
|--------|-------------|--------|
| `sofab.h` (types/constants) | `types`, `error` | ported |
| `ostream.c` | `ostream` ([`OStream`]) | ported |
| `istream.c` | `istream` ([`IStream`] + [`Visitor`]) | ported (push/visitor model instead of bind-target callbacks) |
| `object.c` (descriptor transcoder) | — | not ported. The idiomatic Rust equivalent is a `#[derive(Sofab)]` proc-macro generating `Visitor`/encode glue; the streaming core above already covers serialize/deserialize. |

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

## Testing & coverage

```bash
cargo test --all-features        # unit + integration + doctests
./coverage.sh                    # llvm-cov: terminal summary + HTML + lcov.info
```

Tests live in `tests/` as separate integration files:

- `vectors_tests.rs` — replays the shared `assets/test_vectors.json` (encode, decode, chunked)
- `ostream_tests.rs` — encoder, byte-exact vs. reference vectors
- `istream_tests.rs` — decoder over the same vectors + malformed-input errors
- `roundtrip_tests.rs` — encode→decode value preservation
- `api_tests.rs` — offset reserve, buffer swap, large chunked streaming, API version
- `tests/common/mod.rs` — shared recording `Visitor`

Current coverage: **~93% lines** (`cargo llvm-cov --all-features`).

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
