<p align="center"><img src="assets/sofabuffers_logo.png" alt="SofaBuffers Logo" height="140"></p>

# SofaBuffers

<b>Structured Objects For Anyone</b><br>
<i>... so optimized, feels amazing.</i>

[Would you like to know more?](https://github.com/sofa-buffers)

## SofaBuffers Rust library

[GitHub repository](https://github.com/sofa-buffers/corelib-rs)

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

## Benchmarks

Two runtime benchmarks mirror the C/C++ corelib's `bench/{c,cpp}/` tools — same
messages, same methodology, **identical output format** — so the C, C++ and Rust
implementations can be compared directly. Both are plain binaries
(`harness = false`): no Valgrind or special privileges, runs anywhere `cargo`
does. Throughput is measured against **process CPU time** (`clock_gettime`, not
wall-clock) and `MB = 1e6` bytes.

```bash
cargo bench --bench perf     # per-op cost: cycles/op + MB/s
cargo bench --bench bench    # throughput speedtest: MB/s table
cargo bench                  # both
```

### `benches/perf.rs` — per-op cost (cycles/op + MB/s)

Encodes/decodes one representative message (scalars of every width, integer and
float arrays, a string and a nested sequence) in a ~1 s loop, reporting hardware
**cycles/op** (x86 `_rdtsc` / AArch64 `cntvct_el0`) alongside **CPU-time ns/op**
and **MB/s**. cycles/op tracks the cost of the code; MB/s is this machine's
throughput.

```
--- perf: serialize (stream API) ---
  iterations    : 842517
  message size  : 170 bytes
  cycles/op     : 3317.5  (hardware cycle counter)
  CPU time/op   : 1186.9 ns  (process CPU time, not wall-clock)
  throughput    : 143.2 MB/s  (speedtest, MB = 1e6 bytes)
```

### `benches/bench.rs` — throughput speedtest (MB/s)

Two workloads — a 1000-element `u64` array and a small "typical" mixed message —
each looped ~1 s, encode and decode:

```
Workload                           MB/s
--------                           ----
encode: u64 array (1000)         690.06
encode: typical message           35.83
decode: u64 array (1000)         340.62
decode: typical message           33.34
```

Numbers vary with CPU speed, load and build flags (that's the point — they show
real throughput here). The "typical message" figures are small because, exactly
as in the C/C++ tools, the per-iteration CPU-clock read dominates a sub-100 ns
operation; they are comparable *across languages* but are not an absolute
small-message speed. `black_box` guards keep the optimizer from eliding the work,
and input construction runs outside the timed loop.

## License

MIT (same as the SofaBuffers C corelib).
