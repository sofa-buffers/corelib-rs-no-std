# SofaBuffers `corelib-rs-no-std` — Conformance Gap Analysis & Remediation Plan

Audit of the `#![no_std]` / heap-free Rust port against the language-independent
SofaBuffers specification (`CORELIB_PLAN.md`), with primary focus on the §13
Conformance Checklist. Each item was verified by reading the source, tests,
assets, CI, and devcontainer — not inferred from names.

> Scope note: this audit is read-only. No toolchain is present in the bare
> workspace (`cargo` is unavailable), so the test suite was evaluated by
> inspection rather than executed; all line/byte-level wire claims were checked
> against the source directly.

## Summary

| Status | Count |
|--------|------:|
| PASS    | 11 |
| PARTIAL | 7 |
| GAP     | 0 |
| **Total** | **18** |

There are no hard GAPs (no checklist item is entirely missing). The most
important defect is a wire-level conformance miss: `MAX_DEPTH = 255` is not
enforced. The remaining PARTIALs are mostly fork-hygiene issues — README/manifest
badges, the published crate name, the devcontainer image name, and the CI
toolchain matrix — inherited from the `corelib-rs` (std) port this repo was
branched from.

## Per-checklist-item results

| # | Item (§) | Status | Evidence | Notes |
|---|----------|--------|----------|-------|
| 1 | All public symbols under `sofab` namespace (§6) | PARTIAL | `Cargo.toml:2` `name = "sofab"`, `:13` `[lib] name = "sofab"`; `src/lib.rs:54-67` re-exports under crate `sofab` | Namespace `sofab` is correct, but §6 fixes the **package name** to `SofaBuffers` (distinct from the namespace). The crate name conflates the two — published/installed as `sofab`, not `SofaBuffers`. |
| 2 | API version constant returns `1` (§6) | PASS | `src/types.rs:8` `pub const API_VERSION: u32 = 1`; test `tests/api_tests.rs:84` `api_version_is_one` | Exposed and asserted. |
| 3 | Varint & zig-zag match §4.1–4.2 | PASS | `src/varint.rs:31-66` (incremental decode + overflow guard, zig-zag enc/dec); encode `src/ostream.rs:140-152` | Overflow rejected when shift ≥ value bits (`varint.rs:45`). Zig-zag uses `Signed::BITS` (64 by default). |
| 4 | Field header `(id<<3)\|type` + all 8 wire types (§4.3) | PASS | Pack `src/ostream.rs:154-159`; unpack `src/istream.rs:206-260`; tags `src/types.rs:49-62` | All 8 tags 0x0–0x7 handled on both sides; unknown tag → `InvalidMsg`. |
| 5 | Fixlen word `(len<<3)\|subtype`, LE floats, UTF-8 no terminator, blobs (§4.6) | PASS | Encode `src/ostream.rs:185-218`; decode `src/istream.rs:294-346`; `to_le_bytes`/`from_le_bytes`; `write_str` uses `text.as_bytes()` (no NUL) | fp32/fp64 length validated (must be 4/8). Empty string/blob handled (`istream.rs:333-339`). |
| 6 | Integer arrays + fixlen arrays w/ single shared word; no dynamic subtypes in fixlen arrays (§4.7–4.8) | PASS | Int arrays `src/ostream.rs:227-251`; float arrays write one shared word `:261,:276`; decoder rejects str/blob in array `src/istream.rs:329-332`; count ≥1 enforced encode `:228` / decode `:392` | Empty array → `Error::Argument` (encode), zero count → `InvalidMsg` (decode). |
| 7 | Sequence framing, fresh scope, single-byte `0x07`, skip-by-walking w/ depth, reject nesting > `MAX_DEPTH`=255 (§4.9) | PARTIAL | Framing `src/ostream.rs:286-297`, `src/istream.rs:240-257`; `0x07` end via `write_id_type(0, T_SEQUENCE_END)`; balanced-end check `istream.rs:251` | **`MAX_DEPTH = 255` is not enforced**: `depth` is `u32` and only rejected at `u32::MAX` (`istream.rs:242`). No `MAX_DEPTH` constant exists (grep finds none). Decoder is iterative so no stack risk, but this violates the normative §4.9/§6.2 limit. |
| 8 | Streaming encode into smaller buffer via flush + mid-stream buffer swap (§5.1) | PASS | `OStream::with_flush` `src/ostream.rs:77-85`, `flush` `:95-104`, `buffer_set` `:109-113`, `with_offset` `:61-69`, auto-flush in `push_byte` `:117-130`; tests `api_tests.rs:23` `buffer_set_switches_buffers`, `vectors_tests.rs` chunked-encode 1/3/7-byte buffers | Full coverage incl. start offset. |
| 9 | Streaming decode via `feed` of small chunks, push/pull, lazy bind, auto-skip (§5.2) | PASS | `IStream::feed` byte-at-a-time `src/istream.rs:153-180`; visitor push `:264-407`; auto-skip via default-empty `Visitor` methods `:30-67`; test `istream_tests.rs:156` one-byte + 3-byte feed | Uses the visitor idiom §5.3 explicitly endorses; "pull-read / lazy bind" = the visitor writes the pushed value into its own member and ignores unhandled fields. |
| 10 | Error reporting follows §6.3 baseline codes (§6.3) | PASS | `src/error.rs:9-30`: `Argument`/`Usage`/`BufferFull`/`InvalidMsg`, success = `Ok(())` | All five baseline codes mapped (return-based, no panics/exceptions on hot path — correct for `no_std`). Minor: `Error::Usage` is defined but never constructed (no read type-mismatch path in the push-by-value model). |
| 11 | Streaming primitives sufficient for a thin generated-object layer w/ chunked serialize/deserialize; one-shot helpers thin wrappers (§6.1) | PARTIAL | Encode hook = `OStream` flush-sink + `buffer_set` (`serialize_to`); decode hook = `IStream::feed` + `Visitor`; README "Differences" table states one-shot `decode()` is **std-only** | Encode side fully supports a generated `serialize_to`. Decode side lacks the named `read_sequence` **descend-with-child-handler** hook (§5.2/§6): a generated nested-object decoder must self-track depth in one flat visitor. No one-shot `serialize()/deserialize()` helpers (defensible for heap-free, but the §6.1 hook set is only partially met). |
| 12 | Shared test vectors pass encode+decode, plus chunked, roundtrip, malformed, skip (§7) | PASS | `tests/vectors_tests.rs` (encode, chunked-encode, decode, chunked-decode, `skip_ids`); malformed `tests/istream_tests.rs:186-230`; roundtrip `tests/roundtrip_tests.rs`; `assets/test_vectors.json` is the genuine shared file (`format: sofabuffers-test-vectors`, 67 vectors, groups incl. skip/composite/all arrays) | `requires`-aware so it runs under any feature subset. Not executed (no toolchain); verified by inspection. |
| 13 | `assets/` populated per §8 (branding + `test_vectors.json`) | PASS | `assets/sofabuffers_logo.png` (120 KB), `assets/sofabuffers_icon.png` (6.6 KB), `assets/test_vectors.json` (34 KB) | All three present; JSON header matches the C-generated source of truth. |
| 14 | README family format + badges + required sections (§9) | PARTIAL | `README.md:1-8` header/tagline/org link; badges `:12-14`; "Why this design" `:40-49`; Usage incl. larger-than-buffer `:51-86`; API summary `:88`; Feature flags `:194`; Build & test `:285`; Benchmarks `:347` | Structure complete, **but every badge/link targets `corelib-rs` (the std repo), not `corelib-rs-no-std`** — CI badge `:12`, coverage `:13`, Docs badge `:14` → `sofa-buffers.github.io/corelib-rs/`. `cargo add sofab` `:26` and `Cargo.toml:8 repository = ...corelib-rs` compound the package-name issue from item 1. README also references `tools/footprint.sh` (`:280`) which does not exist in the repo. |
| 15 | `perf` (CPU-independent) + `bench` (MB/s) tools present & runnable (§10) | PASS | `benches/perf.rs` (cycles/op via HW counter + MB/s), `benches/bench.rs` (MB/s); wired `Cargo.toml:27-35` `[[bench]] harness=false` | Both present; README §Benchmarks documents `cargo bench --bench perf|bench`. (No local `BENCH_SPEC.md`, but §10 places that in the family docs repo.) |
| 16 | `.devcontainer/` files + extensions + `.env` gitignored (§11) | PARTIAL | Files all present: `Dockerfile`, `build.sh`, `start.sh`, `attach.sh`, `devcontainer.json`, `.env.example`; `devcontainer.json:10` lists `rust-analyzer`, `even-better-toml`, `anthropic.claude-code`; `.env` not tracked (`git ls-files` shows only `.env.example`), ignored in root `.gitignore:6` and `.devcontainer/.gitignore:5` | **Container naming deviates from the `<lang>-devcontainer` convention.** Per §11.3 the `rs` repo uses `rs-devcontainer`; this repo's `build.sh:6` tags `rust-devcontainer`, and `start.sh:17`/`attach.sh` use container name `sofa-rust-no-std-dev`. Minor: `start.sh:21` bind-mounts `.claude-config` rather than a named `claude-config` volume (§11.1). |
| 17 | `ci.yml` builds & tests on push + PR; version matrix where it matters; coverage uploaded + badge (§12.1) | PARTIAL | `.github/workflows/ci.yml`: triggers push+PR `:3-7`; jobs lint/test/vectors/features/coverage/no_std; coverage badge published to `badges` branch `:142-160`; badge wired in README | **No Rust toolchain-version matrix** — every job pins `dtolnay/rust-toolchain@stable`; §12.1 recommends multiple compiler versions (e.g. stable + beta) and the declared MSRV `Cargo.toml:5` `rust-version = 1.70` is never tested. No explicit **release-profile build** step (§12.1 step 4: debug *and* release). The feature matrix is over Cargo features, not toolchains. Coverage uses a self-hosted Shields endpoint rather than Codecov (acceptable "equivalent"). |
| 18 | `docs.yml` builds HTML docs + Pages via Actions deploy (no `gh-pages`); Docs badge links to site (§12.2) | PARTIAL | `.github/workflows/docs.yml`: `cargo doc --all-features --no-deps`, `upload-pages-artifact@v3`, `deploy-pages@v4`, `permissions: pages/id-token` `:9-12`, push-to-main only `:3-6` | Mechanism is fully correct. **Docs badge target is the wrong repo**: `README.md:14` → `https://sofa-buffers.github.io/corelib-rs/`; this repo deploys to `…/corelib-rs-no-std/`. (Same root cause as item 14.) |

## Remediation Plan

Ordered by severity. No source/behaviour change is applied by this audit — these
are the actions a follow-up PR should take.

### 1. (High) Enforce `MAX_DEPTH = 255` on nested sequences — item 7

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
is publicly visible. A new malformed-input test covers the over-depth case.

### 2. (Medium) Fix package name and all `corelib-rs` → `corelib-rs-no-std` references — items 1, 14, 18

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
- Either add the referenced `tools/footprint.sh` or drop/adjust the README
  reference to it (`README.md:277-283`).

**Files.** `Cargo.toml`, `README.md` (and any doc-link constants in `src/lib.rs`
/ README that hard-code `corelib-rs`).

**Acceptance criteria.** Manifest package name is `SofaBuffers`; import path
stays `sofab`; all README badges/links resolve to `corelib-rs-no-std`; no
dangling reference to a non-existent `tools/footprint.sh`.

### 3. (Medium) Add a Rust toolchain matrix and a release build to CI — item 17

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

### 4. (Medium) Align devcontainer naming with the `<lang>-devcontainer` convention — item 16

**Problem.** §11.3 fixes the image/container name pattern; the `rs` family uses
`rs-devcontainer`. This repo tags `rust-devcontainer` (`build.sh:6`) and runs the
container as `sofa-rust-no-std-dev` (`start.sh:17`, `attach.sh`), so the three
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

### 5. (Low) Round out the generated-object decode hooks — item 11

**Problem.** §6.1 requires the corelib to expose enough hooks for a generated
layer that streams in chunks, explicitly including descending into nested
generated objects via `read_sequence`. The flat `Visitor` delivers
`sequence_begin`/`sequence_end` only as events with no child-handler descend, and
there are no one-shot `serialize()/deserialize()` helpers.

**Fix.**
- Document (or provide) the recommended pattern for a generated nested decoder on
  top of the flat visitor (depth/scope-stack), or add an optional
  child-handler / scoped sub-visitor API so a generated object can hand a nested
  decoder to `read_sequence`.
- Confirm in docs that one-shot `serialize()` (caller-sized buffer) and the
  absence of a heap-allocating one-shot `deserialize()` are intentional
  `no_std` trade-offs.

**Files.** `src/istream.rs`, `src/lib.rs` (docs), `README.md`.

**Acceptance criteria.** A documented, tested path shows a nested generated
object being assembled across `feed` chunks using only the public API; the
`no_std` rationale for omitting a one-shot `deserialize()` is stated.

### 6. (Low) Use or remove the unused `Error::Usage` variant — item 10

**Problem.** `Error::Usage` (`src/error.rs:18`) is part of the §6.3 baseline but
is never constructed; the push-by-value decoder has no read-type-mismatch path.

**Fix.** Keep it for §6.3 parity (preferred — document that it is reserved), or
remove it if strict dead-code hygiene is wanted. Keeping it is recommended for
cross-port consistency.

**Files.** `src/error.rs` (doc comment only, if kept).

**Acceptance criteria.** Either the variant is documented as reserved-for-parity,
or removed with no loss of baseline-code coverage that the port actually emits.
