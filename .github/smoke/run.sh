#!/usr/bin/env bash
# Consume the crate the way a stranger would: from outside the repository, as an
# ordinary dependency, through nothing but its public API.
#
#   CRATE=sofa-buffers-corelib-no-std .github/smoke/run.sh 'path = "/abs/path/to/package"'
#   CRATE=sofa-buffers-corelib-no-std .github/smoke/run.sh 'version = "=0.11.0"'
#
# The argument is the right-hand side of the dependency line, so the same script
# covers both halves of a release: the packaged artifact before the upload, and
# what crates.io actually serves after it. Used by
# `.github/workflows/release.yml`; runnable by hand for the same reason.
#
# Two halves, because neither alone would be enough:
#   * roundtrip.rs runs on the host, where the values can be asserted — but the
#     host has `std`, which would cover for a crate that stopped being no_std.
#   * baremetal.rs is a `#![no_std]` staticlib for a bare-metal target, which
#     proves the link works with no OS, no allocator and no `std` — but nothing
#     there can run. Its exported symbol is checked to still be in the archive,
#     since a linker that dropped the whole crate would make it vacuous.
set -euo pipefail

DEP_SPEC="${1:?usage: run.sh '<cargo dependency spec>'}"
CRATE="${CRATE:?set CRATE to the crates.io package name}"
BARE_TARGET="${BARE_TARGET:-thumbv6m-none-eabi}"   # the smallest target CI builds for
SMOKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# --- host: encode, decode, decode again one byte at a time -------------------
mkdir -p "$WORK/host/src"
cp "$SMOKE_DIR/roundtrip.rs" "$WORK/host/src/main.rs"
cat > "$WORK/host/Cargo.toml" <<EOF
[package]
name = "sofab-smoke"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
$CRATE = { $DEP_SPEC }
EOF

echo "== host smoke: $CRATE { $DEP_SPEC }"
cargo run --release --manifest-path "$WORK/host/Cargo.toml"

# --- bare metal: link it with no std, in the default and minimum configs -----
# baremetal.rs touches only the API that survives `--no-default-features`, so
# both configurations compile the same source.
mkdir -p "$WORK/bare/src"
cp "$SMOKE_DIR/baremetal.rs" "$WORK/bare/src/lib.rs"

NM="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-nm"

for config in default minimal; do
  case "$config" in
    default) defaults="" ;;
    minimal) defaults=", default-features = false" ;;
  esac

  cat > "$WORK/bare/Cargo.toml" <<EOF
[package]
name = "sofab-smoke-baremetal"
version = "0.0.0"
edition = "2021"
publish = false

[lib]
crate-type = ["staticlib"]

[dependencies]
$CRATE = { $DEP_SPEC$defaults }

[profile.release]
panic = "abort"
EOF

  echo "== bare-metal smoke: $BARE_TARGET, $config features"
  cargo build --release --target "$BARE_TARGET" --manifest-path "$WORK/bare/Cargo.toml"

  ARCHIVE="$WORK/bare/target/$BARE_TARGET/release/libsofab_smoke_baremetal.a"
  test -f "$ARCHIVE" || { echo "no archive at $ARCHIVE"; exit 1; }

  if [ -x "$NM" ]; then
    # Read the table into a variable rather than piping into `grep -q`: `-q`
    # exits on the first match, llvm-nm then dies of SIGPIPE, and `pipefail`
    # would report that as a failed check on a perfectly good archive.
    symbols="$("$NM" --defined-only "$ARCHIVE")"
    grep -q ' T sofab_smoke_roundtrip$' <<<"$symbols" \
      || { echo "sofab_smoke_roundtrip is not in the archive — the codec was optimized away, so this check proved nothing"; exit 1; }
    echo "   symbol present: sofab_smoke_roundtrip"
  else
    echo "   note: llvm-nm not found (rustup component llvm-tools-preview) — skipping the symbol check"
  fi
done

echo "smoke ok — host round trip and $BARE_TARGET link, both feature configs"
