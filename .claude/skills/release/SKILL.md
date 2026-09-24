---
name: release
description: Cut a release of sofa-buffers-corelib-no-std — bump Cargo.toml/Cargo.lock/README, land the release commit on main, tag vX.Y.Z, publish the GitHub Release that triggers the crates.io publish. Use when the user asks to release, tag, bump the version or publish to crates.io.
argument-hint: "[X.Y.Z]"
---

# Release `sofa-buffers-corelib-no-std`

The git tag `vX.Y.Z` is the source of truth, but Cargo cannot take the version
from the tag: `cargo publish` reads it from `Cargo.toml`. So the manifest is
bumped **first**, landed on `main`, and only then tagged. Publishing to crates.io
is done by `.github/workflows/release.yml`, triggered by a **published GitHub
Release** (not by the tag push). A crates.io version is immutable — it can only
be yanked, never replaced — so every check happens before that step.

Argument: the target version `X.Y.Z` (without `v`). If missing, propose one
(step 2) and ask.

**Tag format: always a lowercase `v` + the plain semver version**, e.g.
`v1.2.3` — never `1.2.3`, `V1.2.3` or `release-1.2.3`. The version in
`Cargo.toml` carries **no** `v` (`version = "1.2.3"`). The workflows do not
fully enforce this: `version.yml` only fires on `v*` tags, but `release.yml`
strips an optional `v` (`${TAG#v}`), so a bare `1.2.3` tag would slip through
its guard. Check the tag name yourself before creating it:
`[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]`.

**Stop and ask the user before each outward-facing, irreversible step:** pushing
the tag (step 7) and publishing the GitHub Release (step 9).

## 1. Preconditions

```bash
git switch main && git pull -p --ff-only
git status --short                      # must be empty
git describe --tags --abbrev=0          # last tag
curl -s -A release-skill https://crates.io/api/v1/crates/sofa-buffers-corelib-no-std | jq -r '.versions[].num'
gh run list -w ci.yml -b main -L 3      # CI on main must be green
```

The last GitHub Release can lag the last tag (e.g. `v0.11.0` was tagged and
published to crates.io by hand, without a GitHub Release). Take the previous
version from tags **and** crates.io, whichever is higher.

## 2. Choose the version

- Pre-1.0 semver rule (stated in every release note): a **minor** bump may break
  API or wire output; **patch** = fixes/docs/perf without API or wire changes.
- Review what changed: `git log --oneline <last-tag>..main`. Look for `!` in
  commit subjects, `BREAKING`, changed public types in `src/`, wire changes.
- The corelib family releases in lockstep (all corelibs were at `v0.10.0`).
  Check the siblings before choosing a number:
  ```bash
  for r in corelib-c-cpp corelib-cpp corelib-rs corelib-go corelib-ts corelib-py corelib-java corelib-cs corelib-dart corelib-zig; do
    printf '%-16s %s\n' $r "$(gh release list -R sofa-buffers/$r -L1 --json tagName --jq '.[0].tagName')"; done
  ```
  If this release is part of a family-wide release, use the family version.
- The version must not exist yet: `git tag -l vX.Y.Z` empty and crates.io
  `.../sofa-buffers-corelib-no-std/X.Y.Z` → HTTP 404.

## 3. Bump every place the version lives

| File | What | Checked by |
|---|---|---|
| `Cargo.toml` | `[package] version = "X.Y.Z"` | `version.yml` (tag push), `release.yml` guard |
| `Cargo.lock` | the `sofa-buffers-corelib-no-std` entry | `release.yml` guard (`cargo metadata --locked`) |
| `README.md` | the dependency snippet `sofa-buffers-corelib-no-std = { version = "0.N", … }` — set to `"X.Y"` (0.x: major.minor) | nobody — easy to forget |
| `.github/smoke/run.sh`, `.github/workflows/release.yml` | example versions in comments (`=0.11.0`, `v0.11.0`) | cosmetic, optional |

```bash
sed -i '0,/^version = /s/^version = ".*"/version = "X.Y.Z"/' Cargo.toml
cargo update -p sofa-buffers-corelib-no-std   # touches only our own entry in Cargo.lock
git diff --stat                         # Cargo.toml + Cargo.lock (+ README) only
grep -rn 'version = "0\.' README.md     # check the snippet
```

There is **no CHANGELOG.md** (removed in #99). Release notes live only in the
GitHub Release body (step 8). `rust-version` (MSRV 1.70) is not touched by a
release.

## 4. Verify locally

```bash
cargo metadata --locked --format-version 1 >/dev/null   # lockfile in sync
cargo fmt --all --check                                 # same commands as ci.yml
cargo clippy --all-features --all-targets -- -D warnings
cargo test --locked --all-features
cargo test --locked
cargo package --locked                                  # builds the exact .crate
cargo package --locked --list                           # sanity-check the file set
```

## 5. Release commit

Previous practice: branch `release/vX.Y.Z` + PR (see #62), commit message:

```
chore(release): X.Y.Z

The git tag is the source of truth for the version; this brings every package
manifest in line with the vX.Y.Z tag that follows.

<one paragraph: what is breaking since the previous version, if anything>
```

Open the PR (`gh pr create`), wait for CI, merge. `main` is not branch-protected,
so a direct commit on `main` also works if the user prefers it.

## 6. Wait for CI on the exact commit that gets tagged

`release.yml`'s `ci-status` job requires a **successful `ci.yml` run on the
tagged SHA**. A squash/merge creates a new SHA on `main`, so wait for the push run:

```bash
git switch main && git pull -p --ff-only
SHA=$(git rev-parse HEAD)
gh run list -w ci.yml -c "$SHA"          # wait until completed/success (gh run watch <id>)
```

## 7. Tag (ask first)

Annotated tag, message = tag name (as for `v0.11.0`):

```bash
git tag -a vX.Y.Z -m vX.Y.Z "$SHA"
git push origin vX.Y.Z
gh run list -w version.yml -L 1          # "Tag matches Cargo.toml" must pass
```

Optional dress rehearsal — runs guard, ci-status, package + smoke, publishes nothing:

```bash
gh workflow run release.yml -f tag=vX.Y.Z
gh run watch $(gh run list -w release.yml -L1 --json databaseId --jq '.[0].databaseId')
```

## 8. Release notes

Draft from `git log --oneline <prev-tag>..vX.Y.Z`, in the style of the `v0.10.0`
release (`gh release view v0.10.0`):

- one intro line (family alignment if applicable; "the git tag is the source of
  truth for the version; every package manifest matches it")
- **Breaking since vPREV** — under the pre-1.0 rule that a minor bump may break
  API or wire output: bullets with CORELIB_PLAN § / Crucible finding references
- other notable changes (features, fixes, perf, footprint), PR numbers
- write it to a scratch file and show it to the user

## 9. Publish (ask first — this is the irreversible step)

```bash
gh release create vX.Y.Z --verify-tag --title vX.Y.Z --notes-file <notes.md> --latest
```

Publishing the release starts `release.yml`: guard → ci-status + package-smoke
→ `publish` (environment `crates-io`, Trusted Publishing/OIDC, no token needed)
→ `verify` (installs from crates.io, smoke test incl. bare metal).

```bash
gh run watch $(gh run list -w release.yml -L1 --json databaseId --jq '.[0].databaseId')
curl -s -A release-skill https://crates.io/api/v1/crates/sofa-buffers-corelib-no-std/X.Y.Z | jq -r .version.num
```

## When something fails

- **Before `publish` ran** (guard / ci-status / package-smoke red): nothing is
  published. Fix on `main`, then move the tag: `gh release delete vX.Y.Z --yes`,
  `git push --delete origin vX.Y.Z`, `git tag -d vX.Y.Z`, go back to step 6.
- **Guard: "Cargo.toml is at … but the tag says …"** or **"Cargo.lock is out of
  date"** → step 3 was incomplete; fix as above.
- **ci-status: "no successful run for SHA"** → the tag points at a commit CI
  never ran on (or is still running). Wait, or re-tag the right commit.
- **After `publish` succeeded**: the version is final. Never re-tag it. A broken
  release is fixed with a new patch version (and `cargo yank` if necessary —
  ask the user).
- **`verify` red after a successful publish**: check crates.io before re-running;
  the index can lag (the job already polls 5 min).
