<p align="center"><img src="assets/sofabuffers_logo.png" alt="SofaBuffers Logo" height="140"></p>

# SofaBuffers

<b>Structured Objects For Anyone</b><br>
<i>... so optimized, feels amazing.</i>

[Would you like to know more?](https://github.com/sofa-buffers)

## SofaBuffers Rust library

[GitHub repository](https://github.com/sofa-buffers/corelib-rs)

[![CI](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fsofa-buffers%2Fcorelib-rs%2Fbadges%2Fcoverage.json)](https://github.com/sofa-buffers/corelib-rs/actions/workflows/ci.yml)

A `#![no_std]`, **heap-free**, **streaming** Rust implementation of the
SofaBuffers (*Sofab*) serialization format. It is a port of the C `corelib`
(`istream.c` / `ostream.c`) and runs on any platform, from tiny
microcontrollers to desktops and servers.

The wire format is specified, language-neutrally, in the
[SofaBuffers documentation](https://github.com/sofa-buffers/documentation). The
unit tests here use the exact byte vectors from the
[C corelib](https://github.com/sofa-buffers/corelib-c-cpp)'s reference suite
(`test/c/test_ostream.c`) to guarantee byte-for-byte interoperability with the C
implementation.

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

- `ostream_tests.rs` — encoder, byte-exact vs. C vectors
- `istream_tests.rs` — decoder over the same vectors + malformed-input errors
- `roundtrip_tests.rs` — encode→decode value preservation
- `api_tests.rs` — offset reserve, buffer swap, large chunked streaming
- `tests/common/mod.rs` — shared recording `Visitor`

Current coverage: **~93% lines** (`cargo llvm-cov --all-features`).

Coverage prerequisites (one-time):

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
```
