# Changelog

All notable changes to this crate. Versions follow semver with the 0.x rule that
a **minor** bump may break API or wire output.

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
