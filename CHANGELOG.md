# Changelog

All notable changes to this crate. Versions follow semver with the 0.x rule that
a **minor** bump may break API or wire output.

## Unreleased

### Breaking — API

- **Removed `trim_tail`, `trim_tail_f32` and `trim_tail_f64`** (and the `trim`
  module behind them). They implemented the trailing-default elision that
  MESSAGE_SPEC §3 now forbids: under the count-is-capacity amendment a `count: N`
  array carries `0..N` elements, the count prefix on the wire **is** the length,
  and the encoder writes every element the field holds. Nothing fills an array
  back up to `N` on decode, so dropping the tail does not produce a compact
  spelling of the same value — it produces a different, shorter one.
  `trim_tail(&[1, 2, 3, 0, 0], 0)` yielded `[1, 2, 3]`, and encoding that turned
  a five-element array into a three-element array on the wire: silent data loss,
  documented as the canonical form. Generated code has not called them for
  several releases; a hand-written caller that did should pass the array
  **unmodified** to `write_array_unsigned` / `write_array_signed` /
  `write_array_fp32` / `write_array_fp64`, which is both correct and less work.

  The rule is now pinned by tests (`tests/no_elision_tests.rs`): every array
  flavour keeps its trailing defaults on the wire and through a round-trip — the
  shared vector `array_unsigned_trailing_defaults` covers the same ground — and
  the crate surface is checked to expose no such helper again.

### Added

- **A `Flush` sink can now take the buffer it was handed and install a
  replacement.** CORELIB_PLAN §5.1 makes both shapes of the returning-callback
  contract expressible — return without installing (the sink *copied*, the
  encoder resumes at `0` in the same buffer), or take the buffer and install a
  replacement before returning — and only the first existed here. A callback
  receives `&mut self` and a borrowed `&[u8]` with no handle on the stream (the
  stream is mutably borrowed by the very call that invoked it), so
  `OStream::buffer_set` was unreachable from inside it and the zero-copy
  hand-off the buffer-set operation exists for — encode straight into the
  packet, hand the packet on, encode the next into another — could not be built
  on this port at all. Neither could a per-packet framing-header reservation,
  since the start offset belongs to the installation.

  The new `Handover` channel carries the replacement instead: the caller creates
  it, hands it to the new `OStream::with_handover(buffer, offset, sink,
  &handover)`, and shares it with the sink, which calls `handover.install(next,
  offset)` from inside the callback and picks the buffer it took back up with
  `handover.taken()` — the encoder gives up that borrow when it installs the
  replacement, which is what lets a pool recycle it. `install` is checked
  exactly like every other installation (offset in range,
  `len - offset >= MIN_OUTPUT_BUFFER`, `Error::Argument` **where the buffer is
  handed over**), and a rejected buffer leaves the active one in place.

  **Free where it is not used.** `OStream` gained a third type parameter for the
  channel, defaulted to the zero-sized `NoHandoff` whose `TAKES` is a
  compile-time `false`, so `OStream::new` / `with_flush` streams fold the whole
  take-and-replace path away: `size_of::<OStream>()` is unchanged in every
  configuration and the Cortex-M0 flash figures are byte-identical to the
  previous release on all eight footprint rows. Existing code is unaffected —
  the parameter is defaulted, `Flush` is untouched, and generic code written as
  `OStream<'_, F>` still names the same type.

### Fixed

- **`cargo bench --bench bench` — the invocation the README documents — exited
  with "unknown workload: --bench".** The throughput tool reads the optional
  Callgrind workload name from `argv[1]`, but `benches/bench.rs` is a
  `harness = false` bench target, so cargo appends its own `--bench` flag to
  every run: the tool saw a workload named `--bench`, printed the error and
  exited 2 before measuring anything. Plain `cargo bench` (both tools) failed
  the same way; only running the built binary by hand, or
  `benches/run_callgrind.sh` (which passes a bare workload name), still worked.
  The workload is now the first *non-flag* argument, so cargo's flags fall
  through to the full table while `bench <workload>` keeps selecting a single
  op. No library code is involved — the crate itself is unchanged. The selection
  lives in `benches/support/workload_arg.rs`, is covered by
  `tests/bench_tools_tests.rs` in the normal suite, and a new `Bench tools` CI
  job runs both documented commands plus every single-op workload, so a broken
  tool cannot ship green again (CORELIB_PLAN §10/§13).

- **`write_fixlen(id, bytes, FixlenType::Str)` emitted an unchecked non-UTF-8
  `string` field.** This port declares itself pinned to the ON state of
  `SOFAB_STRICT_UTF8` (CORELIB_PLAN §6.4) and exposes no validator, on the
  strength of §6.4's exemption for Unicode-string targets — an exemption that
  rests on the encode *API* being unable to accept bytes for a `string` field.
  `write_str` honoured that, but it was a wrapper over the public, byte-taking
  `write_fixlen`, whose `FixlenType::Str` arm handed arbitrary bytes straight to
  the wire: `write_fixlen(1, &[0xFF, 0xFE], FixlenType::Str)` returned `Ok(())`
  and produced `0a 12 ff fe`, a `string` field that this family's own strict
  decoders reject as `INVALID`. `write_fixlen` now refuses `FixlenType::Str`
  with `Error::Argument` **before a byte reaches the buffer** — the encoder is
  untouched, so the next write encodes exactly as if the call had not been made
  — and stays the primitive for the byte-shaped subtypes `fp32` / `fp64` /
  `blob`. `write_str` and its `&str` are the only door to a `string` field,
  which is what makes "strict by construction" true of the whole API rather than
  of one method.

  **Breaking** only for a hand-written caller that passed bytes for a `string`:
  validate them once with `core::str::from_utf8` and call `write_str`, or use
  `write_blob` if they are not text. Generated code is unaffected — the Rust
  backend emits `write_str`. The check is one comparison on a subtype the
  internal callers pass as a constant, so it folds away for them; flash and RAM
  are **unchanged on every footprint row** and there is no allocation, no panic
  path and no UTF-8 validator pulled into the image.

- **An unbalanced sequence close reported success and emitted a message the
  decoder rejects.** `write_sequence_end` / `write_sequence_end_keep` with no
  open sequence returned `Ok(())` and wrote a bare `0x07`, papering the depth
  underflow over with `saturating_sub`. A sequence-end marker with no open
  sequence is one of the `INVALID` conditions of CORELIB_PLAN §5.2, so those
  bytes fed back into this crate's own `IStream` were `Err(InvalidMsg)` — the
  encoder produced, with a success status, output the caller had not asked for
  (§5.1). Both closers now return `Error::Argument` when nothing is open,
  writing nothing and leaving the encoder untouched; the depth counter is the
  same one that already gated the open side, so this is one comparison on a cold
  path — no new state, no allocation, no panic path, RAM unchanged in every
  configuration and **+8 B of flash** on the `sequence`-enabled rows (±4 B on the
  generated-shape visitor row). The README footprint table is regenerated
  accordingly.

- **A message proven malformed could still deliver fields.** The `INVALID`
  decode outcome is terminal (CORELIB_PLAN §5.2), but `IStream` kept no record
  of having returned `Error::InvalidMsg`: it recomputed the verdict from the
  current state on every `feed`, and most `INVALID` conditions — a dangling
  sequence end, an overlong varint, an over-maximum id — leave the state machine
  at a clean field boundary. The next `feed` therefore parsed on, pushed the
  following fields to the visitor and returned `Ok(())` (`COMPLETE`) for a byte
  stream the decoder itself had already rejected. `feed(&[0x07, 0x08, 0x2a])`
  was `InvalidMsg` with nothing delivered, while `feed(&[0x07])` then
  `feed(&[0x08, 0x2a])` was `InvalidMsg` and then `Ok(())` with `unsigned(1, 42)`
  delivered — the outcome depended on where the chunk boundary fell, which §7.2
  item 4 forbids. `INVALID` is now latched: every later `feed` returns
  `InvalidMsg` without consuming the chunk or delivering a field, and decoding
  another message means a fresh `IStream::new()`. The latch is a terminal state
  of the decoder's existing state byte rather than a flag of its own — an extra
  byte would be free in `Core`'s padding but not in flash, since it perturbs the
  initializer image `IStream::new` stores (~180 B of `.text` on a 64-bit-value
  build, measured). `size_of::<IStream>()` is unchanged (32 B with every feature
  on), RAM is unchanged in every configuration, flash moves by at most +32 B on
  one row, and there is no new allocation and no new panic path. The README
  footprint table is regenerated accordingly.

- **A stale output buffer could be flushed downstream as message content.**
  `OStream::with_flush` / `buffer_set` accepted an `offset` past the end of the
  buffer. The first write then saw `offset >= len`, handed the buffer's entire
  previous content to the sink as if those bytes were part of the message, and
  resumed at 0 — silently prepending garbage. Installing a 4-byte buffer at
  offset 9 produced `ee ee ee ee 08 2a` where the one-shot encoding is `08 2a`.
- **`OStream::flush()` could panic.** With a sink and `offset > buffer.len()` it
  sliced past the buffer (`range end index 9 out of range for slice of length
  4`). A reachable panic contradicts this crate's `#![forbid(unsafe_code)]` /
  no-`core::panicking` footprint guarantee, and on bare metal it is a hard fault.
  The slice is now clamped as well as unreachable.

Both had one root cause: no installation path validated the buffer/offset pair.

- **A mid-stream `buffer_set` under a flush sink discarded the buffered bytes.**
  `OStream::buffer_set` overwrote the buffer pointer and the cursor without
  draining, so with a sink installed everything written since the last flush was
  dropped: it never reached the sink, the caller no longer owned the buffer it
  was in, and the call still returned `Ok(())`. Writing field 1, swapping
  buffers, writing field 2 emitted `10 07` where the one-shot encoding is
  `08 2a 10 07` — a silently truncated message, which CORELIB_PLAN §5.1 forbids
  ("MUST produce output byte-identical to the one-shot path"). The swap now
  drains to the sink first, after the buffer/offset check so a rejected swap
  stays a no-op, and writing resumes at the new installation's `offset`.
  `Flush::SINKS` is a compile-time constant, so a `NoFlush` encoder folds the
  drain away entirely: with no sink the caller still owns the old buffer and the
  `BufferFull` recovery path (install a bigger buffer, retry the failed write) is
  unchanged, byte for byte. No new allocation, no new panic path.

### Added

- **`MIN_OUTPUT_BUFFER` (= `1`)** — the smallest buffer this port accepts *for
  streaming*, now declared, documented and enforced as CORELIB_PLAN §5.1
  requires. This encoder splits every atomic unit (one single-byte push
  primitive that flushes and resumes on its own), so it declares the strictest
  value the spec allows — the footprint-profile choice — and imposes no
  requirement on its caller beyond a non-empty window.

  It binds a buffer installed **with a flush sink**, at installation and at
  every mid-stream buffer-set, and **nothing else**: a buffer installed without
  a sink is subject to no minimum, since no flush can occur and the buffer
  either holds the message or reports `BufferFull`. Both halves are covered by
  the §7.2 item 4 tests (`tests/api_tests.rs`), including an encode through a
  window of exactly `MIN_OUTPUT_BUFFER` asserted byte-identical to the one-shot
  output over a payload far longer than the window.

### Changed

**Breaking** — the buffer-installation paths are now fallible, which is what
§5.1 means by "rejected where it is handed over, by the same mechanism the port
uses for an out-of-range offset":

| before | after |
|---|---|
| `OStream::with_offset(buf, off) -> Self` | `-> Result<Self>` |
| `OStream::with_flush(buf, off, sink) -> Self` | `-> Result<Self>` |
| `OStream::buffer_set(buf, off) -> ()` | `-> Result<()>` |

All three return `Error::Argument` for an `offset` past the end of the buffer;
the two sink-carrying ones also reject `buffer.len() - offset <
MIN_OUTPUT_BUFFER`. A rejected `buffer_set` is a no-op — the previous buffer
stays installed, so a refused swap cannot strand the encoder or lose written
bytes. `OStream::new` is unchanged and stays infallible: its cursor starts at 0,
which is in range for every buffer, and it installs no sink.

Migration is a `?` or `.unwrap()` at each call site; no wire output changes.

### Added

- **`Visitor::fixlen_begin(id, subtype, total)`** — a scalar fixlen field is now
  announced at its **length word**, after the word is read and validated and
  before any payload byte, exactly as an array is announced at its count word
  through `array_begin` (issue #68, CORELIB_PLAN §5.2). It fires once per scalar
  fixlen field, `total == 0` included, and never for an array element.

  This closes an ordering gap: the only event carrying a string/blob `total` was
  `Visitor::string` / `Visitor::blob` on the payload path, which cannot fire for
  a message truncated exactly at the length word. A `maxlen` violation is fully
  established by that word, and §5.2 makes **INVALID dominate INCOMPLETE** — so
  without this hook `[1a, 52]` (tag + length word for an over-bound string, no
  payload) degraded to INCOMPLETE, while the same bytes read whole are INVALID, a
  chunk-boundary-dependent verdict §6.4/§7.2 forbid. The hook lets generated code
  latch the violation at the word.

  Additive and non-breaking: a default-empty trait method (like `array_begin`),
  monomorphized away for any visitor that does not override it, so it costs a
  `no_std` consumer nothing it does not already opt into. No existing visitor
  changes behavior; no wire output changes.

## 0.10.0 - 2026-08-01

### Breaking — API

`ArrayKind` gained `Fp32` and `Fp64` and lost the collapsed `Fixlen`, and the
array-header hook now fires **after** a fixlen array's `fixlen_word` so `kind`
names the real element subtype (CORELIB_PLAN §4.8, Crucible F-0042). A consumer
can therefore skip a header whose subtype contradicts the declared element type
*before* any schema `count` bound is applied to it. Generated code is the
expected consumer and sofabgen changed in lockstep.

## 0.9.0

Implements MESSAGE_SPEC §2 as amended by
[documentation#29](https://github.com/sofa-buffers/documentation/pull/29): a
sequence-typed **field** equal to its declared default is *omitted*, not framed
empty. Both the API and the bytes this crate emits change, hence the minor bump
from 0.8.0.

### Breaking — API

- **Removed `OStream::write_sequence_begin`** (the eager opener). Sequences are
  now opened with **`write_sequence_begin_lazy`**, which holds the header back
  until the sequence turns out to have content. Every existing caller of
  `write_sequence_begin` fails to compile; the migration is mechanical:

  | before | after |
  |---|---|
  | `write_sequence_begin(id)` + `write_sequence_end()` | `write_sequence_begin_lazy(id)` + `write_sequence_end()` — for a `struct`/`union` field and an array *wrapper*: an all-default one now vanishes |
  | `write_sequence_begin(id)` + `write_sequence_end()` | `write_sequence_begin_lazy(id)` + **`write_sequence_end_keep()`** — for a wrapper-array *element*, whose presence carries the array's length (§5.1), and for an array field encoding "explicitly empty" against a non-empty declared default |

  Choosing the closer is a static property of the position in the schema, not of
  the value; see the README's "Sequence framing, and the hold-back window".

- **Added `OStream::write_sequence_end_keep`** and the public constant
  **`LAZY_SEQ_DEPTH`** (= 8), the depth to which headers are held back on this
  heap-free profile (CORELIB_PLAN §6). It is fixed at build time — no feature,
  `cfg` or environment variable changes it.

- **`ArrayKind::Fixlen` is replaced by `ArrayKind::Fp32` / `ArrayKind::Fp64`**,
  and `Visitor::array_begin` now fires **after** a fixlen array's `fixlen_word`
  instead of on its count varint (Crucible finding **F-0042**, CORELIB_PLAN
  §4.8). The signature is unchanged; only the kind's domain and the fixlen call
  site move. Migration: match on the two subtypes where you matched `Fixlen`.

  Both halves are needed by the same rule. §4.8 decides a fixlen array's element
  subtype *before* the field is acted on: a subtype that contradicts the
  declared element type makes the whole field a §7.3 skip, and a schema `count`
  bound must **not** be applied to it — so a consumer has to know fp32-vs-fp64
  at the moment it is asked about the array, and must not be asked before the
  subtype exists. Consequences, both intended: a message truncated *between* the
  count word and the `fixlen_word` is now `INCOMPLETE` (it was judged on the
  count alone), and an over-long array with a contradicting subtype is a skip
  rather than a rejection. Integer arrays are untouched — they have no second
  word, so their header still fires on the count varint — and the `ARRAY_MAX`
  format ceiling still fires there for *every* array kind, before anything is
  announced. A zero-count fixlen array is still announced exactly once, with the
  subtype from its word.

### Breaking — wire output

- An all-default sequence **field** no longer reaches the wire, so an all-default
  message is now the **empty byte string**. Decoders are unaffected: the empty
  frame stays valid input and normalizes to the same value (§2).
- Nested deeper than `LAZY_SEQ_DEPTH`, a contentless sequence still keeps its
  empty frame — well-formed, decodes identically, just not canonical.

### Performance

Measured, not asserted — instruction counts (callgrind, `bench` profile,
x86-64), 0.8.0 (commit `9bdcc16`) → this version:

| workload | 0.8.0 | 0.9.0 | change |
|---|---:|---:|---:|
| encode, typical message (37 B) | 484 Ir/op | 701 Ir/op | **+44.8 %** |
| decode, typical message | 7 066 Ir/op | 7 067 Ir/op | +1 Ir/op |
| encode, `u64[1000]` | 138 853 Ir/op | 135 855 Ir/op | −2.2 % |
| decode, `u64[1000]` | 486 153 Ir/op | 486 153 Ir/op | unchanged |

Flash, the primary axis, moves too: the `+ sequence` row of `tools/footprint.sh`
goes 902 B → 1 130 B on Cortex-M0 (+228 B) and `OStream` grows 16 B → 52 B of
RAM — the hold-back array. The README's footprint tables are regenerated.

The small-message encode is the regression to know about: lazy framing adds a
pending-run test to **every** field write (the `write_id_type` choke point) and a
cold, never-inlined `commit_pending` call per non-default sequence, and that call
boundary also blocks optimizations the eager encoder allowed. Output is
byte-identical for this message (37 B) — the cost buys the omission of all-default
sequences elsewhere. On this footprint profile throughput is the secondary axis
(the primary one, `.text`, is in the README's footprint table), but the number
belongs in writing rather than in the next reader's profiler.

Both columns were measured with the *same* harness — the 0.8.0 tree with this
version's `benches/perf.rs` — so the comparison is of the codec, not of the
benchmark. That harness also gained a `black_box` on the encode destination
buffer: without it the optimizer hoists the whole encode workload out of the
measurement loop (on 0.8.0 it did, reporting ~0 Ir/op).

Reproduce (all four numbers come from the crate's own `perf` bench, whose
adaptive 1 s loop can be pinned to a fixed iteration count so a profiler sees a
deterministic run):

```bash
cargo bench --bench perf --all-features --no-run     # prints the binary path
for n in 20000 120000; do
  SOFAB_PERF_ONLY=encode SOFAB_PERF_ITERS=$n \
    valgrind --tool=callgrind --callgrind-out-file=/dev/null <binary>
done
# per-op cost = (Ir(120000) - Ir(20000)) / 100000
```

`SOFAB_PERF_ONLY` (`encode` / `decode` / `encode_u64` / `decode_u64`) and
`SOFAB_PERF_ITERS` are new in this version and affect the bench tool only.

## 0.8.0 and earlier

Not tracked in this file; see the git history.
