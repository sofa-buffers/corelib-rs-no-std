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

### String validity (strict UTF-8)

A `string` field is UTF-8. Rust's `str`/`String` is a **Unicode string type**,
so this port is **always strict** — the `SOFAB_STRICT_UTF8` option
(CORELIB_PLAN §6.4) is a **no-op here, pinned ON**, and there is no primitive to
expose (only byte-container targets need one):

- **Encode is strict by construction.** `OStream::write_str` takes `&str`, which
  is already guaranteed valid UTF-8 by the type system, so a `string` field can
  never carry invalid bytes — no runtime check is possible or needed. Put
  arbitrary bytes in a `blob` (`write_blob`).
- **Decode strictness lives in generated code.** The corelib hands a `string`
  field's **raw bytes** to `Visitor::string` and never builds a `str`/`String`;
  generated code materializes it with `core::str::from_utf8`, turning invalid
  bytes into `Error::InvalidMsg` (the `INVALID` decode outcome). Invalid UTF-8 is
  **rejected, never replaced** with `U+FFFD` or truncated. Embedded `U+0000` is
  valid UTF-8 and round-trips byte-exact. std (`corelib-rs`) and no_std agree
  (subsumes generator #80).

The shared `invalid_utf8` negative vectors in `assets/test_vectors.json`
(tracking corelib-c-cpp#97) are exercised by `tests/utf8_tests.rs` (needs the
default `fixlen` feature).

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
    })
    .unwrap();                                // rejects a window below MIN_OUTPUT_BUFFER
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

### Sequence framing, and the hold-back window

A nested message is a **sequence**: `write_sequence_begin_lazy(id)` opens one and
a closer ends it. MESSAGE_SPEC §2 **omits a sequence-typed field whose value
equals its declared default**, and "not one child was written" is exactly that
condition — but the header has to be on the wire *before* the children that
decide it. Rather than buffer the sub-message (impossible without a heap), the
encoder **holds the header back**: the ids of the innermost open sequences form a
pending run, the first field write emits the whole run outermost-first, and the
closer decides what an empty one does.

| call | effect |
|------|--------|
| `write_sequence_begin_lazy(id)` | open a scope, hold its header back (no bytes, no allocation) |
| `write_sequence_end()` | got no content → **drop it**, header and end marker both |
| `write_sequence_end_keep()` | emit the run *and* the end marker, so an empty sequence still reaches the wire as `begin`+`end` |

Which closer to use is a **static** property of the position in the schema, not
of the value: `end` for a `struct`/`union` field and for an array *wrapper*;
`end_keep` for a wrapper-array **element**, whose presence is what carries a
dynamic array's length (§5.1), and for an array field that must encode
"explicitly empty" against a non-empty declared default. Getting it wrong is not
symmetric — `end_keep` where `end` would do costs one non-canonical empty frame
that every decoder normalizes away, while the reverse silently changes an
array's decoded length. So "always framed" is false for a **field** and still
true for an **element**.

```rust
let mut buf = [0u8; 32];
let mut os = sofab::OStream::new(&mut buf);
os.write_sequence_begin_lazy(1).unwrap();   // a struct field that stays all-default
os.write_sequence_end().unwrap();
assert_eq!(os.bytes_used(), 0);             // omitted entirely: zero bytes
```

**The bound: `LAZY_SEQ_DEPTH = 8`.** The pending run lives in a fixed array
inside `OStream` — this port has no heap to grow one. CORELIB_PLAN §6 ("How deep
the hold-back reaches") lets a **heap-free profile** bound the run and requires
it to document the bound, because two encoders that disagree about it disagree
about *bytes*, not about validity:

- Up to **8** nested sequences are held back, and an all-default one at any of
  those depths is **omitted** — the canonical §2 encoding.
- Open a 9th while eight are pending and the encoder **commits the run and frames
  that sequence eagerly**. An all-default sequence beyond the window therefore
  keeps its empty `begin`+`end` frame: still well-formed, still decodes to the
  same value (it is the non-canonical form §2 already requires every decoder to
  accept and normalize) — just not canonical. Ports that can allocate
  (`corelib-rs` and the rest) hold back to the full `MAX_DEPTH` and are canonical
  at every depth.
- `MAX_DEPTH` (255) is unaffected: it still bounds the nesting itself, and
  exceeding it is `Error::Argument`.

The window is the price in RAM, which is why it is 8 and not 255: on Cortex-M0
the pending array grows `OStream` from **16 B to 52 B** (`4 * LAZY_SEQ_DEPTH`
plus the count) — see the RAM table under [Footprint](#footprint), where the
`sequence`-enabled rows carry exactly that cost. A schema nesting deeper than the
window still encodes **correctly**, it only keeps the empty frames beyond it.

The value is **not configurable**: `sofab::LAZY_SEQ_DEPTH` is a public constant to
read and test against, but no Cargo feature, `cfg` or environment variable
changes it — every build of this crate holds back 8. A firmware that nests only
two or three levels deep and wants those RAM bytes back has to edit the constant
in a patched or vendored copy of the crate; that changes which bytes the encoder
emits, so the window tests in
[`tests/ostream_tests.rs`](tests/ostream_tests.rs) have to be re-stated with it.

**If the buffer runs out while a run is committing.** Held-back ids are encoder
state, not buffer content, so no flush can split a run *before* it commits. One
can land in the middle of one, and with a `Flush` sink that is uneventful — the
bytes go to the sink and the run carries on. Without a sink the same point is
`Error::BufferFull`, possibly *between* two headers of a single run, and the
encoder then **keeps the ids it did not emit**, still as the innermost pending
suffix. Install a bigger buffer with `buffer_set` and retry the failed write: it
resumes at the cut. That recovery reaches exactly as far as the rest of this
encoder does and no further — no writer here is atomic on failure, so a cut that
falls *inside* a multi-byte header (id > 15), or inside any other varint, still
leaves a partial message behind, exactly as it does for a scalar field. What is
guaranteed is the structural half: a run never silently drops a `SEQUENCE_START`
whose `SEQUENCE_END` still gets written.

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
- **`MIN_OUTPUT_BUFFER` = 1** — the smallest buffer this port accepts **for
  streaming**, i.e. one installed *together with* a `Flush` sink. Every byte goes
  through a single-byte push primitive that flushes and resumes on its own, so no
  atomic unit has to land contiguously: a message of any size streams through a
  one-byte window and produces bytes identical to the one-shot encoding. The
  constant binds `OStream::with_flush` and `OStream::buffer_set`, which require
  `buffer.len() - offset >= MIN_OUTPUT_BUFFER` and return `Error::Argument`
  **where the buffer is handed over**, never partway through a message. An
  out-of-range `offset` is rejected the same way on every installation path,
  including `OStream::with_offset`.
- **A buffer installed without a sink is subject to no minimum.** No flush can
  occur, so the buffer either holds the message or reports `Error::BufferFull` —
  size it from your message's worst case and it stays exact (a two-byte message
  encodes into a two-byte buffer).
- **No pass-through.** This port never hands a sink memory that is not the
  installed output buffer; a `string`/`blob` run is copied through the buffer
  like anything else, so a sink may assume every slice it receives points into
  the buffer it installed.
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
Line coverage is ~92% (`cargo llvm-cov --all-features`). To exercise the whole
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

**Instruction counts.** Cycles and MB/s are noisy across machines, so the numbers
quoted in the [changelog](CHANGELOG.md) are callgrind instruction counts, which
are deterministic. Two environment variables make `perf` measurable that way, and
change nothing otherwise: `SOFAB_PERF_ONLY` (`encode` / `decode` / `encode_u64` /
`decode_u64`) runs a single workload, and `SOFAB_PERF_ITERS=N` replaces the
adaptive ~1 s loop with exactly N iterations. Run at two N and difference the
totals — start-up and warm-up cancel:

```bash
cargo bench --bench perf --all-features --no-run   # prints the binary path
for n in 20000 120000; do
  SOFAB_PERF_ONLY=encode SOFAB_PERF_ITERS=$n \
    valgrind --tool=callgrind --callgrind-out-file=/dev/null <binary>
done                       # per-op Ir = (Ir(120000) - Ir(20000)) / 100000
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
| **MIN** — integers only, 32-bit (`default-features = false`) | **626 B** | **638 B** | **780 B** |
| integers only, 64-bit (`value64`) | 804 B | 826 B | 952 B |
| `+ sequence` (64-bit) | 1 128 B | 1 142 B | 1 442 B |
| `+ array` (64-bit) | 1 100 B | 1 100 B | 1 260 B |
| `+ fixlen` (fp32 / str / blob, 64-bit) | 1 161 B | 1 191 B | 1 433 B |
| all wire types, 32-bit | 1 931 B | 1 911 B | 2 501 B |
| **MAX** — all wire types, 64-bit (default) | **2 201 B** | **2 131 B** | **2 797 B** |
| generated-shape visitor (MAX) | 4 283 B | 4 199 B | 5 293 B |

The `sequence` rows carry the lazy-framing machinery of MESSAGE_SPEC §2 (the
hold-back run, [above](#sequence-framing-and-the-hold-back-window)): 226 B of
flash on Cortex-M0 over an eager `begin`/`end` pair (1 128 B against the 902 B
the same row measured before lazy framing), plus the pending array's RAM in the
table below. About 60 B of that is `commit_pending` tracking how much
of the run reached the buffer so a `BufferFull` in the middle of one keeps the
ids it did not emit ([above](#sequence-framing-and-the-hold-back-window)) — the
price of not emitting a `SEQUENCE_END` whose `SEQUENCE_START` was dropped.

The codec spans **≈0.6 KiB** (integer-only, 32-bit) to **≈2.1 KiB** (every wire
type, 64-bit) of flash on Cortex-M0; disabling `value64` removes ~12% of the code
by deleting the 64-bit shift helpers and halving every varint operation. The decoder carries no panic paths (all bounds are proven in-bounds),
so the whole codec links without `core::panicking` — which is what keeps the
RISC-V builds, lacking Thumb-2's density, close behind Cortex-M.

The **generated-shape visitor** row measures the same MAX build against a
visitor mirroring sofabgen output (location stack, per-`(location, id)`
dispatch, fixed-array fills, str/blob accumulation) instead of the tiny probe
sink — the codec-plus-dispatch cost a real firmware actually links. Size
changes must hold on this row, not just the probe-sink rows: the two can move
in opposite directions when an inlining boundary shifts.

**RAM.** There is no heap and no static RAM — the only runtime state is the
caller-provided `IStream` (decoder) and `OStream` (encoder), usually stack
allocated. Sizes are identical across these 32-bit targets:

| Configuration | `IStream` | `OStream` | total |
|---------------|----------:|----------:|------:|
| **MIN** — integers only, 32-bit | 12 B | 12 B | **24 B** |
| integers only, 64-bit | 24 B | 12 B | 36 B |
| `+ sequence` (64-bit) | 24 B | 52 B | 76 B |
| `+ array` (64-bit) | 24 B | 12 B | 36 B |
| `+ fixlen` (64-bit) | 32 B | 12 B | 44 B |
| all wire types, 32-bit | 32 B | 52 B | 84 B |
| **MAX** — all wire types, 64-bit (default) | 32 B | 52 B | **84 B** |

The decoder state is held at **32 bytes or less** in every configuration on
purpose, and that is a flash figure as much as a RAM one: at or below that size
the compiler zero-initializes an `IStream` with inline stores, while a larger one
links a ~158-byte `__aeabi_memclr8` helper that a lean firmware needs for nothing
else.

The `sequence` rows are where `OStream` grows from 12/16 B to 52 B: that is the
`LAZY_SEQ_DEPTH`-slot hold-back array
([above](#sequence-framing-and-the-hold-back-window)) — `4 * 8` bytes of ids plus
the count. It is the only per-stream cost of omitting all-default sequences, and
it is fixed at build time — see
[the bound](#sequence-framing-and-the-hold-back-window) for what a target that
cannot spare it has to do.

## Choosing between the two Rust corelibs

SofaBuffers ships **two** Rust cores with the same wire format and the same
encoder/decoder API, tuned for opposite ends of the spectrum:

- **`corelib-rs-no-std`** (this crate) — `#![no_std]`, no allocator, fixed
  caller buffers, size-optimized profile. For **microcontrollers and
  footprint-constrained firmware**. In the multi-language arena it runs at
  roughly **1.4× micropb** per-message throughput while fitting a bare-metal
  Cortex-M image of about **6.8 KB flash versus micropb's ~8.5 KB**.
- **[`corelib-rs`](https://github.com/sofa-buffers/corelib-rs)** — the `std`
  port, `opt-level = 3`, allocates freely (owned `String`/`Vec`, one-shot
  `decode()`). For **servers and desktops** wanting maximum throughput and
  ergonomic ownership; roughly **1.5× prost** per-message throughput.

| | `corelib-rs-no-std` (this crate) | `corelib-rs` (`std`) |
|---|---|---|
| Target | microcontrollers → servers | desktop / server |
| `std` / allocator | neither (`#![no_std]`, no `alloc`) | requires `std` |
| Buffers | caller-owned fixed capacity | library-allocated (`String`/`Vec`) |
| Decode model | push to a `Visitor`, zero-copy `chunk` views | owning one-shot `decode()` |
| Release profile | `opt-level = "z"`, LTO, `panic = "abort"` | `opt-level = 3`, LTO |
| Optimized for | small `.text` + zero heap | raw throughput |
| Arena result | ~1.4× micropb throughput; ~6.8 KB Cortex-M flash | ~1.5× prost throughput |

Both crates run the **identical** `perf` and `bench` tools. In the multi-language
[arena](https://github.com/sofa-buffers/arena) (best-of-5, encode+decode roundtrip
of the same 434 B message) the size-tuned `no_std` build trails the speed-tuned
`std` build by roughly 2×:

| Workload | `no_std` MB/s | `std` MB/s | `std` faster |
| --- | ---: | ---: | ---: |
| typical message — encode + decode roundtrip (434 B) | 158.6 | 341.1 | 2.15× |

That is the deliberate trade-off: pick this crate for embedded and footprint —
where the `std` crate cannot build at all — and pick `corelib-rs` for servers
and throughput.
