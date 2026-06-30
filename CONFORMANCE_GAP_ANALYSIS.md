# SofaBuffers `corelib-rs-no-std` — Conformance Gap Analysis & Remediation Plan

Audit of the `#![no_std]` / heap-free Rust port against the language-independent
SofaBuffers specification (`CORELIB_PLAN.md`), with primary focus on the §13
Conformance Checklist. Each item was verified by reading the source, tests,
assets, CI, and devcontainer — not inferred from names.

> Scope note: this audit is read-only. No toolchain is present in the bare
> workspace (`cargo` is unavailable), so the test suite was evaluated by
> inspection rather than executed; all line/byte-level wire claims were checked
> against the source directly.

## Spec revision

This is a **refresh** of a prior audit against an updated spec.

- **Spec basis:** updated `CORELIB_PLAN.md` (commit `dcb85d6`), §13 Conformance
  Checklist plus all §4–§12 detail. Refreshed **2026-06-30**.
- **Headline change:** zero-length arrays and empty sequences are now **legal**
  wire encodings; a port that rejects them (on encode or decode) is now
  **non-conformant**. Specifically:
  - §4.7: `element_count` range is now `0 .. 2,147,483,647` (was `1..`). A
    zero-count integer array is valid and is exactly
    `[ header_varint ] [ element_count_varint = 0 ]`, nothing after.
    Absent-vs-empty is now a code-generator concern, not a wire-level one.
  - §4.8: a zero-count fixlen array (fp32/fp64) has **no `fixlen_word` and no
    payload** — exactly `[ header_varint ] [ element_count_varint = 0 ]`.
  - §4.9: an empty sequence (`sequence start` immediately followed by `0x07`
    end) is legal and a decoder **must** accept it.

### What changed vs previous revision

- **Item 6 (§4.7–4.8 arrays): PASS → GAP.** The previous audit credited the port
  for enforcing `count >= 1` on both encode and decode. Under the updated spec
  that enforcement is exactly the non-conformance: the encoder rejects empty
  arrays with `Error::Argument` and the decoder rejects `count == 0` with
  `Error::InvalidMsg`. This is now the single most severe defect (it rejects
  legal wire data on **both** sides and diverges from every other port).
- **Item 7 (§4.9 sequences): still PARTIAL, but reason narrowed.** The newly
  mandated **empty sequence** is already accepted by both encoder and decoder
  (verified below and covered by shared vectors), so it is *not* a gap. The
  PARTIAL now rests **solely** on the still-missing `MAX_DEPTH = 255`
  enforcement (carried forward unchanged from the previous audit).
- **Item 12 (§7 tests): PASS → PARTIAL.** Two local unit tests now assert the
  now-non-conformant reject-zero-count behavior, and the shared suite has no
  zero-count *array* vector, so the array regression is uncovered. (The shared
  suite *does* carry empty-sequence vectors, which pass.)
- **Carried forward unchanged** (spec change does not touch these areas):
  MAX_DEPTH gap (item 7), package name `sofab` vs required `SofaBuffers` +
  `corelib-rs` wrong-repo references (items 1, 14, 18), CI has no MSRV/toolchain
  matrix and no release build (item 17), devcontainer naming off-convention
  (item 16), generated-object decode hooks (item 11), unused `Error::Usage`
  (item 10).
- **Corrected from previous audit (unrelated to spec change):** the prior note
  that `tools/footprint.sh` "does not exist in the repo" was wrong — the file
  **is** present (`tools/footprint.sh`); that sub-finding is dropped.

### Zero-length / empty-sequence support — how it actually looks today

| Case | Encoder | Decoder | Shared vectors | Verdict |
|------|---------|---------|----------------|---------|
| Zero-count **unsigned** array | **Rejected** `Error::Argument` (`src/ostream.rs:228-229`) | **Rejected** `Error::InvalidMsg` (`src/istream.rs:392`) | none | **non-conformant** |
| Zero-count **signed** array | **Rejected** `Error::Argument` (`src/ostream.rs:242-243`) | **Rejected** `Error::InvalidMsg` (`src/istream.rs:392`) | none | **non-conformant** |
| Zero-count **fixlen** array (no `fixlen_word`/payload) | **Rejected** `Error::Argument` (`src/ostream.rs:256-258`, `:272-273`); also unconditionally emits the `fixlen_word` (`:261`, `:276`) | **Rejected** `Error::InvalidMsg` (`src/istream.rs:392`); a count-0 path would also wrongly descend into `FixlenLen` expecting a word | none | **non-conformant** |
| Empty **sequence** (`start` then `0x07`) | Allowed — `write_sequence_begin`+`write_sequence_end` have no restriction (`src/ostream.rs:288-297`) | Accepted — depth `0→1→0`, fires `sequence_begin`/`sequence_end` (`src/istream.rs:241-257`) | `empty_sequence`, `nested_empty_sequences`, `empty_sequence_between_fields` present and pass | **conformant** |

## Summary

| Status | Count | Δ vs previous |
|--------|------:|:-------------:|
| PASS    | 9  | −2 |
| PARTIAL | 8  | +1 |
| GAP     | 1  | +1 |
| **Total** | **18** | |

The new hard GAP (item 6) is a wire-level conformance miss introduced by the
spec change: legal zero-count arrays are rejected on both encode and decode.
The second wire-level defect, `MAX_DEPTH = 255` not being enforced (item 7),
carries forward. The remaining PARTIALs are mostly fork-hygiene issues —
README/manifest badges, the published crate name, the devcontainer image name,
and the CI toolchain matrix — inherited from the `corelib-rs` (std) port this
repo was branched from.

## Per-checklist-item results

| # | Item (§) | Status | Evidence | Notes |
|---|----------|--------|----------|-------|
| 1 | All public symbols under `sofab` namespace (§6) | PARTIAL | `Cargo.toml:2` `name = "sofab"`, `:13` `[lib] name = "sofab"`; `src/lib.rs:67` re-exports under crate `sofab` | Namespace `sofab` is correct, but §6 fixes the **package name** to `SofaBuffers` (distinct from the namespace). The crate name conflates the two — published/installed as `sofab`, not `SofaBuffers`. |
| 2 | API version constant returns `1` (§6) | PASS | `src/types.rs:8` `pub const API_VERSION: u32 = 1`; test `tests/api_tests.rs` `api_version_is_one` | Exposed and asserted. |
| 3 | Varint & zig-zag match §4.1–4.2 | PASS | decode `src/varint.rs` (incremental + overflow guard, zig-zag enc/dec); encode `src/ostream.rs:140-152` | Overflow rejected (`varint_overflow_is_invalid`, `tests/istream_tests.rs:193`). Zig-zag uses 64-bit width by default. |
| 4 | Field header `(id<<3)\|type` + all 8 wire types (§4.3) | PASS | Pack `src/ostream.rs:154-159`; unpack `src/istream.rs:200-260`; tags `src/types.rs:49-62` | All 8 tags 0x0–0x7 handled on both sides; unknown tag → `InvalidMsg` (`istream.rs:259`). |
| 5 | Fixlen word `(len<<3)\|subtype`, LE floats, UTF-8 no terminator, blobs (§4.6) | PASS | Encode `src/ostream.rs:185-218`; decode `src/istream.rs:294-346`; `to_le_bytes`/`from_le_bytes`; `write_str` uses `text.as_bytes()` (no NUL) | fp32/fp64 length validated (must be 4/8, `istream.rs:315`/`:322`). Empty string/blob handled (`istream.rs:333-339`). |
| 6 | Integer arrays + fixlen arrays w/ single shared word; **zero-count arrays valid**; no dynamic subtypes in fixlen arrays (§4.7–4.8) | **GAP** | Encode rejects empty: `src/ostream.rs:228-229,242-243,256-258,272-273`; decode rejects `count==0`: `src/istream.rs:392`; tests lock it in: `tests/ostream_tests.rs:304` `empty_array_is_argument_error`, `tests/istream_tests.rs:187` `array_count_zero_is_invalid` | **Non-conformant under the updated §4.7–4.8.** All three array writers reject empty input with `Error::Argument`; the decoder rejects a zero-count header with `Error::InvalidMsg`. For a fixlen array the encoder also writes the `fixlen_word` unconditionally (`:261`/`:276`) and the decoder always descends into `FixlenLen` — both wrong for §4.8's word-less/payload-less zero-count form. Non-zero arrays and the no-dynamic-subtype rule (`istream.rs:329-332`) remain correct. |
| 7 | Sequence framing, fresh scope, single-byte `0x07`, **empty sequence accepted**, skip-by-walking w/ depth, reject nesting > `MAX_DEPTH`=255 (§4.9) | PARTIAL | Framing `src/ostream.rs:286-297`, `src/istream.rs:240-257`; `0x07` end via `write_id_type(0, T_SEQUENCE_END)`; balanced-end check `istream.rs:251`; empty-sequence vectors pass (`assets/test_vectors.json`: `empty_sequence`/`nested_empty_sequences`/`empty_sequence_between_fields`) | Empty sequence (new §4.9 requirement) is **conformant** — encoder permits it, decoder accepts it (depth `0→1→0`). **`MAX_DEPTH = 255` is still not enforced**: `depth` is `u32` and only rejected at `u32::MAX` (`istream.rs:242`); no `MAX_DEPTH` constant exists. Decoder is iterative (no stack risk) but this violates the normative §4.9/§6.2 limit. This is now the *only* reason item 7 is not PASS. |
| 8 | Streaming encode into smaller buffer via flush + mid-stream buffer swap (§5.1) | PASS | `OStream::with_flush` `src/ostream.rs:77-85`, `flush` `:95-104`, `buffer_set` `:109-113`, `with_offset` `:61-69`, auto-flush in `push_byte` `:117-130`; chunked-encode tests in `tests/vectors_tests.rs` + `flush_sink_streams_large_message` `tests/ostream_tests.rs:313` | Full coverage incl. start offset. |
| 9 | Streaming decode via `feed` of small chunks, push/pull, lazy bind, auto-skip (§5.2) | PASS | `IStream::feed` byte-at-a-time `src/istream.rs:153-180`; visitor push `:264-407`; auto-skip via default-empty `Visitor` methods `:30-67`; one-byte/odd-chunk feed tests in `tests/istream_tests.rs` & `tests/vectors_tests.rs` | Uses the visitor idiom §5.3 endorses; "pull-read / lazy bind" = the visitor writes the pushed value into its own member and ignores unhandled fields. |
| 10 | Error reporting follows §6.3 baseline codes (§6.3) | PASS | `src/error.rs`: `Argument`/`Usage`/`BufferFull`/`InvalidMsg`, success = `Ok(())` | All baseline codes mapped (return-based, no panics/exceptions on hot path — correct for `no_std`). Minor: `Error::Usage` is defined but never constructed (no read type-mismatch path in the push-by-value model). |
| 11 | Streaming primitives sufficient for a thin generated-object layer w/ chunked serialize/deserialize; one-shot helpers thin wrappers (§6.1) | PARTIAL | Encode hook = `OStream` flush-sink + `buffer_set` (`serialize_to`); decode hook = `IStream::feed` + `Visitor`; README "Differences" notes one-shot `decode()` is **std-only** | Encode side fully supports a generated `serialize_to`. Decode side lacks a named `read_sequence` **descend-with-child-handler** hook (§5.2/§6): a generated nested-object decoder must self-track depth in one flat visitor. No one-shot `serialize()/deserialize()` helpers (defensible for heap-free, but the §6.1 hook set is only partially met). |
| 12 | Shared test vectors pass encode+decode, plus chunked, roundtrip, malformed, skip (§7) | PARTIAL | `tests/vectors_tests.rs` (encode, chunked-encode, decode, chunked-decode, `skip_ids`); malformed `tests/istream_tests.rs:186-230`; roundtrip `tests/roundtrip_tests.rs`; `assets/test_vectors.json` (67 vectors, incl. the three empty-sequence vectors) | All 67 shared vectors still encode/decode (empty-sequence vectors pass). **But** under the updated spec the suite has two problems: (a) no shared or local vector exercises a **zero-count array**, so the item-6 regression is uncovered; (b) two local "malformed" tests now assert legal data is invalid — `tests/ostream_tests.rs:304` and `tests/istream_tests.rs:187` codify the now-non-conformant reject-zero-count behavior and must be updated/removed. Not executed (no toolchain); verified by inspection. |
| 13 | `assets/` populated per §8 (branding + `test_vectors.json`) | PASS | `assets/sofabuffers_logo.png`, `assets/sofabuffers_icon.png`, `assets/test_vectors.json` (`format: sofabuffers-test-vectors`, version 1, 67 vectors) | All three present; JSON header matches the C-generated source of truth. |
| 14 | README family format + badges + required sections (§9) | PARTIAL | `README.md` header/tagline/org link; badges `:12-14`; "Why this design"; Usage incl. larger-than-buffer; API summary; Feature flags; Build & test; Benchmarks | Structure complete, **but every badge/link targets `corelib-rs` (the std repo), not `corelib-rs-no-std`** — CI badge `:12`, coverage `:13`, Docs badge `:14` → `sofa-buffers.github.io/corelib-rs/`; `[GitHub repository]` `:16` and `cargo add sofab` `:26`. Compounds the package-name issue from item 1. (`tools/footprint.sh`, referenced by the README, does exist in the repo.) |
| 15 | `perf` (CPU-independent) + `bench` (MB/s) tools present & runnable (§10) | PASS | `benches/perf.rs`, `benches/bench.rs`; wired `Cargo.toml` `[[bench]] harness=false` | Both present; README §Benchmarks documents `cargo bench --bench perf|bench`. (No local `BENCH_SPEC.md`, but §10 places that in the family docs repo.) |
| 16 | `.devcontainer/` files + extensions + `.env` gitignored (§11) | PARTIAL | Files all present: `Dockerfile`, `build.sh`, `start.sh`, `attach.sh`, `devcontainer.json`, `.env.example`; `devcontainer.json` lists `rust-analyzer`, `even-better-toml`, `anthropic.claude-code`; `.env` not tracked, ignored in `.gitignore` and `.devcontainer/.gitignore` | **Container naming deviates from the `<lang>-devcontainer` convention.** Per §11.3 the `rs` repo uses `rs-devcontainer`; this repo's `build.sh:6` tags `rust-devcontainer`, and `start.sh:17`/`attach.sh:4` use container name `sofa-rust-no-std-dev`. Minor: `start.sh` bind-mounts `.claude-config` rather than a named `claude-config` volume (§11.1). |
| 17 | `ci.yml` builds & tests on push + PR; version matrix where it matters; coverage uploaded + badge (§12.1) | PARTIAL | `.github/workflows/ci.yml`: triggers push+PR; jobs lint/test/vectors/features/coverage/no_std; coverage badge published to `badges` branch; badge wired in README | **No Rust toolchain-version matrix** — every job pins `dtolnay/rust-toolchain@stable` (`:20,:42,:76,:91,:122,:169`); §12.1 recommends multiple compiler versions (e.g. stable + beta) and the declared MSRV `Cargo.toml:5` `rust-version = 1.70` is never tested. No explicit **release-profile build** step (§12.1 step 4: debug *and* release). The existing matrix (`:61`) is over Cargo features, not toolchains. Coverage uses a self-hosted Shields endpoint rather than Codecov (acceptable "equivalent"). |
| 18 | `docs.yml` builds HTML docs + Pages via Actions deploy (no `gh-pages`); Docs badge links to site (§12.2) | PARTIAL | `.github/workflows/docs.yml`: `cargo doc`, `upload-pages-artifact@v3`, `deploy-pages@v4`, `permissions: pages/id-token`, push-to-main only | Mechanism is fully correct. **Docs badge target is the wrong repo**: `README.md:14` → `https://sofa-buffers.github.io/corelib-rs/`; this repo deploys to `…/corelib-rs-no-std/`. (Same root cause as item 14.) |

## Remediation Plan

Ordered by severity. No source/behaviour change is applied by this audit — these
are the actions a follow-up PR should take.

### 1. (High) Accept zero-count arrays on encode AND decode, incl. word-less fixlen arrays — item 6 (NEW)

**Problem.** Updated §4.7–4.8 make a zero-count array a valid, fully-specified
empty array on the wire. The port rejects it on both sides: all three array
writers return `Error::Argument` for empty input (`src/ostream.rs:228-229`,
`:242-243`, `:256-258`, `:272-273`), and the decoder rejects a `count == 0`
header with `Error::InvalidMsg` (`src/istream.rs:392`). For fixlen arrays §4.8
additionally requires that a zero-count array carry **no `fixlen_word` and no
payload** — but the encoder writes the word unconditionally (`:261`, `:276`) and
the decoder always transitions into `FixlenLen` expecting one. This rejects legal
messages and diverges from every other port.

**Fix.**
- Encoder: drop the `is_empty()` → `Error::Argument` guards in
  `write_array_unsigned`/`write_array_signed`/`write_array_fp32`/`write_array_fp64`.
  Emit `[ header ] [ count_varint = 0 ]` and **stop** — for fixlen arrays do
  **not** emit the `fixlen_word` or any payload when the slice is empty.
- Decoder `step_array_count` (`src/istream.rs:386-407`): on `count == 0`, fire
  `visitor.array_begin(id, kind, 0)` and return to `State::Idle` directly —
  do **not** descend into `VarintUnsigned`/`VarintSigned`/`FixlenLen` (in
  particular, do not try to read a `fixlen_word` for a zero-count fixlen array).
  Keep the `count > ARRAY_MAX` upper-bound check.

**Files.** `src/ostream.rs`, `src/istream.rs`, plus tests (see remediation #6).

**Acceptance criteria.** Encoding an empty `u8`/`i32`/`fp32`/`fp64` array yields
exactly `[ header ] [ 0x00 ]`; decoding those bytes fires a single
`array_begin(.., count = 0)` and resumes cleanly on the next field; a zero-count
fixlen array round-trips with no `fixlen_word`.

### 2. (High) Enforce `MAX_DEPTH = 255` on nested sequences — item 7 (carried forward)

**Problem.** §4.9/§6.2 mandate a maximum nesting depth of 255 and require the
decoder to reject deeper nesting with `InvalidMessage`. The port tracks depth in
a `u32` and only fails at `u32::MAX` (`src/istream.rs:242`); no `MAX_DEPTH`
constant exists. A message nesting 256+ sequences decodes successfully —
non-conformant with the wire spec, and a divergence from every other port.

**Fix.**
- Add `pub const MAX_DEPTH: u32 = 255;` to `src/types.rs` and re-export it from
  `src/lib.rs` (it belongs to the §6.2 normative limit set).
- In `IStream::step_idle` `T_SEQUENCE_START` (`src/istream.rs:240-248`), return
  `Err(Error::InvalidMsg)` when `self.depth >= MAX_DEPTH` instead of the current
  `u32::MAX` guard.
- Optionally have the encoder track depth and return an error on the 256th
  `write_sequence_begin` (§4.9 "an encoder must not open more than 255").

**Files.** `src/types.rs`, `src/lib.rs`, `src/istream.rs`, `src/ostream.rs`
(optional), `tests/istream_tests.rs` (new test).

**Acceptance criteria.** A decoder fed 256 nested `sequence_begin` markers
returns `Error::InvalidMsg`; 255 levels still decode. `sofab::MAX_DEPTH == 255`
is publicly visible.

### 3. (Medium) Fix package name and all `corelib-rs` → `corelib-rs-no-std` references — items 1, 14, 18

**Problem.** The repo was forked from the std port and still identifies as it.
§6 requires the package name `SofaBuffers` (namespace stays `sofab`), but
`Cargo.toml:2` publishes as `sofab`. Every README badge and the `Cargo.toml:8`
`repository` URL point at `corelib-rs`, so the CI, coverage, and Docs badges link
to the wrong project; the Docs badge (`README.md:14`) points at a Pages site this
repo does not own.

**Fix.**
- `Cargo.toml`: set the published package name to `SofaBuffers` while keeping
  `[lib] name = "sofab"` (users install `SofaBuffers`, `use sofab::…`); update
  `repository` to `…/corelib-rs-no-std`.
- `README.md`: repoint the CI, coverage, and Docs badges and the "GitHub
  repository" link to `corelib-rs-no-std`; update the `sofa-buffers.github.io`
  Docs URL to `…/corelib-rs-no-std/`; change `cargo add sofab` to
  `cargo add SofaBuffers`.

**Files.** `Cargo.toml`, `README.md`.

**Acceptance criteria.** Manifest package name is `SofaBuffers`; import path
stays `sofab`; all README badges/links resolve to `corelib-rs-no-std`.

### 4. (Medium) Add a Rust toolchain matrix and a release build to CI — item 17

**Problem.** §12.1 recommends, for compiler-versioned languages, testing across
multiple toolchains; the declared MSRV (`Cargo.toml:5` = 1.70) is never exercised
and CI pins `@stable` everywhere. CI also never builds the release profile
(§12.1 step 4 asks for debug *and* release).

**Fix.**
- Give the `test` job a `strategy.matrix` over toolchains (e.g.
  `["stable", "beta", "1.70"]`) with `fail-fast: false`, driving
  `dtolnay/rust-toolchain@${{ matrix.toolchain }}`.
- Add a `cargo build --all-features --release` step (the size-optimized profile
  is the shipping config for this crate).

**Files.** `.github/workflows/ci.yml`.

**Acceptance criteria.** CI runs the suite on stable, beta, and the MSRV with
visible per-leg results, and builds both debug and release; a regression that
breaks MSRV 1.70 fails CI.

### 5. (Medium) Align devcontainer naming with the `<lang>-devcontainer` convention — item 16

**Problem.** §11.3 fixes the image/container name pattern; the `rs` family uses
`rs-devcontainer`. This repo tags `rust-devcontainer` (`build.sh:6`) and runs the
container as `sofa-rust-no-std-dev` (`start.sh:17`, `attach.sh:4`), so the three
scripts are mutually inconsistent and off-convention.

**Fix.** Pick one name following the convention (`rs-devcontainer`, or
`rs-no-std-devcontainer` to disambiguate from the std port) and use it
consistently as the image tag in `build.sh`, the `--name` in `start.sh`, and the
target in `attach.sh`. Optionally switch `start.sh` to a named `claude-config`
volume per §11.1.

**Files.** `.devcontainer/build.sh`, `.devcontainer/start.sh`,
`.devcontainer/attach.sh`.

**Acceptance criteria.** `build.sh`, `start.sh`, and `attach.sh` reference one
consistent name matching the `<lang>-devcontainer` pattern; `attach.sh` attaches
to the container `start.sh` launches.

### 6. (Low) Realign tests with zero-count rules and add zero-count-array coverage — item 12 (NEW)

**Problem.** Under the updated spec two local tests now assert that legal data is
malformed: `tests/ostream_tests.rs:304` `empty_array_is_argument_error` (expects
`Error::Argument`) and `tests/istream_tests.rs:187` `array_count_zero_is_invalid`
(expects `Error::InvalidMsg`). No shared or local vector exercises a zero-count
array, so the item-6 regression would otherwise pass CI unnoticed.

**Fix.**
- After remediation #1, replace those two tests with positive cases: encoding an
  empty array produces `[ header ] [ 0x00 ]`; decoding it yields a single
  `array_begin(.., 0)` then resumes. Cover unsigned, signed, and fixlen
  (word-less) arrays.
- If/when the C source-of-truth regenerates `test_vectors.json` with zero-count
  array vectors, re-copy it into `assets/` (do not hand-author divergent
  vectors). The empty-sequence vectors are already present and pass.

**Files.** `tests/ostream_tests.rs`, `tests/istream_tests.rs`,
`assets/test_vectors.json` (refresh from `corelib-c-cpp` when available).

**Acceptance criteria.** No local test asserts that a zero-count array is an
error; positive zero-count round-trip tests exist for all three array kinds.

### 7. (Low) Round out the generated-object decode hooks — item 11

**Problem.** §6.1 requires the corelib to expose enough hooks for a generated
layer that streams in chunks, explicitly including descending into nested
generated objects via `read_sequence`. The flat `Visitor` delivers
`sequence_begin`/`sequence_end` only as events with no child-handler descend, and
there are no one-shot `serialize()/deserialize()` helpers.

**Fix.** Document (or provide) the recommended pattern for a generated nested
decoder on top of the flat visitor (depth/scope-stack), or add an optional
child-handler / scoped sub-visitor API. Confirm in docs that one-shot
`serialize()` (caller-sized buffer) and the absence of a heap-allocating one-shot
`deserialize()` are intentional `no_std` trade-offs.

**Files.** `src/istream.rs`, `src/lib.rs` (docs), `README.md`.

**Acceptance criteria.** A documented, tested path shows a nested generated
object being assembled across `feed` chunks using only the public API; the
`no_std` rationale for omitting a one-shot `deserialize()` is stated.

### 8. (Low) Use or document the unused `Error::Usage` variant — item 10

**Problem.** `Error::Usage` (`src/error.rs`) is part of the §6.3 baseline but is
never constructed; the push-by-value decoder has no read-type-mismatch path.

**Fix.** Keep it for §6.3 parity (preferred — document that it is reserved), or
remove it if strict dead-code hygiene is wanted. Keeping it is recommended for
cross-port consistency.

**Files.** `src/error.rs` (doc comment only, if kept).

**Acceptance criteria.** Either the variant is documented as reserved-for-parity,
or removed with no loss of baseline-code coverage that the port actually emits.
