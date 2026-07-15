#!/usr/bin/env bash
# Measure the flash + RAM footprint of the sofab library on a bare-metal target
# (default: Cortex-M0, thumbv6m-none-eabi) across feature configurations.
#
# Three numbers are reported per configuration:
#   * flash  — `.text + .data` of the linked image: code, read-only constants and
#              any initialised statics that occupy flash. (`.text` alone matches
#              the README table; the library defines no statics, so flash = .text.)
#   * RAM    — `size_of::<IStream>() + size_of::<OStream>()`: the decoder + encoder
#              state the caller must provide. The library has **no** static RAM
#              (`.bss`/`.data` = 0) and no heap, so this is the whole RAM cost.
#
# How it works: we generate a throwaway `no_std` staticlib that calls the whole
# encode + decode API, build it with the size-optimized release profile, then
# LINK it with `rust-lld --gc-sections` so unreachable code is stripped, and read
# the real section sizes from the linked ELF with `llvm-size`. A bare staticlib
# archive is NOT dead-stripped, so measuring it directly massively over-counts;
# the link step is what makes the code numbers meaningful. The struct sizes come
# from two zero-cost probe symbols read out of the archive with `llvm-nm
# --print-size`; they are unreferenced, so `--gc-sections` drops them from the
# linked image (they never touch the flash/RAM figures above).
#
# Prereqs (one-time):
#   rustup target add thumbv6m-none-eabi
#   rustup component add llvm-tools-preview
#
# Usage: tools/footprint.sh [target-triple]
set -euo pipefail

TARGET="${1:-thumbv6m-none-eabi}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SR="$(rustc --print sysroot)"
BIN="$SR/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin"
SIZE="$BIN/llvm-size"
NM="$BIN/llvm-nm"
LLD="$BIN/rust-lld"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/src"

cat > "$WORK/Cargo.toml" <<EOF
[package]
name = "sofab_footprint"
version = "0.0.0"
edition = "2021"
[lib]
crate-type = ["staticlib"]
[dependencies]
sofab = { path = "$REPO", default-features = false, package = "sofa-buffers-corelib-no-std" }
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
[features]
fixlen   = ["sofab/fixlen"]
array    = ["sofab/array"]
sequence = ["sofab/sequence"]
fp64     = ["sofab/fp64"]
value64  = ["sofab/value64"]
EOF

cat > "$WORK/src/lib.rs" <<'EOF'
#![no_std]
use core::mem::size_of;
use core::panic::PanicInfo;
use sofab::{IStream, NoFlush, OStream, Visitor};
#[panic_handler] fn ph(_: &PanicInfo) -> ! { loop {} }
// Probe symbols whose byte SIZE equals the decoder/encoder state a caller must
// provide (read from the archive with `llvm-nm --print-size`). They are never
// referenced, so `--gc-sections` strips them from the linked image.
#[no_mangle] pub static SOFAB_ISTREAM_RAM: [u8; size_of::<IStream>()] = [0; size_of::<IStream>()];
#[no_mangle] pub static SOFAB_OSTREAM_RAM: [u8; size_of::<OStream<'static, NoFlush>>()] = [0; size_of::<OStream<'static, NoFlush>>()];
struct Sink { u: u64, i: i64 }
impl Visitor for Sink {
    fn unsigned(&mut self, _i: sofab::Id, v: sofab::Unsigned) { self.u = self.u.wrapping_add(v as u64); }
    fn signed(&mut self, _i: sofab::Id, v: sofab::Signed) { self.i = self.i.wrapping_add(v as i64); }
    #[cfg(feature = "fixlen")] fn fp32(&mut self, _i: sofab::Id, v: f32) { self.u = self.u.wrapping_add(v.to_bits() as u64); }
    #[cfg(feature = "fp64")]   fn fp64(&mut self, _i: sofab::Id, v: f64) { self.u = self.u.wrapping_add(v.to_bits()); }
    #[cfg(feature = "fixlen")] fn string(&mut self, _i: sofab::Id, t: usize, _o: usize, _c: &[u8]) { self.u = self.u.wrapping_add(t as u64); }
    #[cfg(feature = "fixlen")] fn blob(&mut self, _i: sofab::Id, t: usize, _o: usize, _c: &[u8]) { self.u = self.u.wrapping_add(t as u64); }
    #[cfg(feature = "array")]  fn array_begin(&mut self, _i: sofab::Id, _k: sofab::ArrayKind, c: usize) { self.u = self.u.wrapping_add(c as u64); }
    #[cfg(feature = "sequence")] fn sequence_begin(&mut self, _i: sofab::Id) { self.u = self.u.wrapping_add(1); }
    #[cfg(feature = "sequence")] fn sequence_end(&mut self) { self.u = self.u.wrapping_add(2); }
}
#[no_mangle] pub extern "C" fn enc(buf: *mut u8, len: usize, a: u64, b: i64) -> usize {
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let mut os = OStream::new(buf);
    let _ = os.write_unsigned(1, a as sofab::Unsigned);
    let _ = os.write_signed(2, b as sofab::Signed);
    let _ = os.write_boolean(3, a != 0);
    #[cfg(feature = "fixlen")] { let _ = os.write_fp32(4, f32::from_bits(a as u32)); let _ = os.write_str(6, "hi"); let _ = os.write_blob(7, &[1,2,3]); }
    #[cfg(feature = "fp64")]   { let _ = os.write_fp64(5, f64::from_bits(a)); }
    #[cfg(feature = "array")]  { let _ = os.write_array_unsigned(8, &[a as u32, 1, 2]); let _ = os.write_array_signed(9, &[b as i32, -1, 2]); }
    #[cfg(all(feature = "array", feature = "fixlen"))] { let _ = os.write_array_fp32(10, &[f32::from_bits(a as u32), f32::from_bits(1)]); }
    #[cfg(all(feature = "array", feature = "fp64"))]   { let _ = os.write_array_fp64(11, &[f64::from_bits(a), f64::from_bits(1)]); }
    #[cfg(feature = "sequence")] { let _ = os.write_sequence_begin(12); let _ = os.write_sequence_end(); }
    os.bytes_used()
}
#[no_mangle] pub extern "C" fn dec(buf: *const u8, len: usize) -> u64 {
    let data = unsafe { core::slice::from_raw_parts(buf, len) };
    let mut s = Sink { u: 0, i: 0 };
    let mut is = IStream::new();
    let _ = is.feed(data, &mut s);
    s.u.wrapping_add(s.i as u64)
}
#[no_mangle] pub extern "C" fn reset() -> ! {
    let mut buf = [0u8; 128];
    let a = unsafe { core::ptr::read_volatile(0x2000_1000 as *const u64) };
    let b = unsafe { core::ptr::read_volatile(0x2000_1008 as *const i64) };
    let n = enc(buf.as_mut_ptr(), buf.len(), a, b);
    let s = dec(buf.as_ptr(), n);
    unsafe { core::ptr::write_volatile(0x2000_0000 as *mut u64, s) };
    loop {}
}
EOF

cat > "$WORK/link.x" <<'EOF'
MEMORY { FLASH (rx): ORIGIN = 0, LENGTH = 256K  RAM (rwx): ORIGIN = 0x20000000, LENGTH = 64K }
ENTRY(reset)
SECTIONS {
  .text : { KEEP(*(.vectors)) *(.text .text.*) *(.rodata .rodata.*) } > FLASH
  .data : { *(.data .data.*) } > RAM AT> FLASH
  .bss  : { *(.bss .bss.*) }  > RAM
  /DISCARD/ : { *(.ARM.exidx*) *(.comment) }
}
EOF

ARCHIVE="$WORK/target/$TARGET/release/libsofab_footprint.a"

measure() { # label  feature-list (empty = integers only)
  local label="$1" feats="$2"
  ( cd "$WORK"
    rm -rf target out.elf
    if [ -z "$feats" ]; then
      cargo build --release --target "$TARGET" --quiet
    else
      cargo build --release --target "$TARGET" --no-default-features --features "$feats" --quiet
    fi
    "$LLD" -flavor gnu -T link.x --gc-sections -o out.elf --whole-archive \
      "target/$TARGET/release/libsofab_footprint.a"
  )
  # Berkeley `size`: columns are text / data / bss. Flash = text + data; the
  # library carries no statics, so bss is 0 and flash == .text.
  local text data bss
  read -r text data bss < <("$SIZE" "$WORK/out.elf" | awk 'NR==2{print $1, $2, $3}')
  local flash=$((text + data))
  # Caller-provided state RAM, read as the probe symbols' sizes. `--radix=d`
  # zero-pads, so force base-10 (`10#`) to avoid octal misparsing.
  local is os ram nm_out
  nm_out=$("$NM" --print-size --radix=d "$ARCHIVE" 2>/dev/null)
  is=$((10#$(awk '$NF=="SOFAB_ISTREAM_RAM"{print $2; exit}' <<<"$nm_out")))
  os=$((10#$(awk '$NF=="SOFAB_OSTREAM_RAM"{print $2; exit}' <<<"$nm_out")))
  ram=$((is + os))
  printf "  %-38s %6d B   %5d B  (IStream %2d + OStream %2d)\n" \
    "$label" "$flash" "$ram" "$is" "$os"
}

# Builds are --no-default-features, so a config is exactly the features listed.
# Omitting `value64` selects the 32-bit value width.
echo "sofab footprint on $TARGET (opt-z, LTO, panic=abort, gc-sections)"
printf "  %-38s %8s   %7s\n" "configuration" "flash" "RAM"
measure "MIN: integers only, 32-bit"        ""
measure "integers only, 64-bit"             "value64"
measure "+ sequence (64-bit)"               "value64,sequence"
measure "+ array (64-bit)"                  "value64,array"
measure "+ fixlen (64-bit)"                 "value64,fixlen"
measure "all wire types, 32-bit"            "fixlen,array,sequence,fp64"
measure "MAX: all wire types, 64-bit"       "value64,fixlen,array,sequence,fp64"
