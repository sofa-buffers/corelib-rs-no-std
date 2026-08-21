//! Bare-metal smoke test for the *packaged* crate, run by
//! `.github/workflows/release.yml`.
//!
//! `roundtrip.rs` runs the API on the host, where `std` is present and would
//! quietly cover for a crate that had stopped being `no_std`. This one is a
//! `#![no_std]` staticlib built for a bare-metal target: it links with no
//! operating system, no allocator and its own panic handler, which is the
//! property the whole crate exists for and the one a host build cannot check.
//!
//! It uses **only** the API that survives `--no-default-features` (integers),
//! so the workflow can build it in the minimum feature configuration as well as
//! the default one.

#![no_std]

use core::panic::PanicInfo;
use sofab::{IStream, Id, OStream, Signed, Unsigned, Visitor};

#[derive(Default)]
struct Probe {
    a: Unsigned,
    b: Signed,
}

impl Visitor for Probe {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        if id == 1 {
            self.a = v;
        }
    }
    fn signed(&mut self, id: Id, v: Signed) {
        if id == 2 {
            self.b = v;
        }
    }
}

/// Round-trips two scalars through the codec. Returns 0 on success and a
/// non-zero code identifying the step that failed — the caller is a linker,
/// not a test harness, so the contract is a return value rather than a panic.
///
/// `#[unsafe(no_mangle)]` keeps the symbol in the archive: without it nothing
/// references this code and the linker is free to drop the whole crate, which
/// would make the check vacuous.
#[unsafe(no_mangle)]
pub extern "C" fn sofab_smoke_roundtrip() -> u32 {
    let mut buf = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut buf);
        if os.write_unsigned(1, 42).is_err() {
            return 1;
        }
        if os.write_signed(2, -7).is_err() {
            return 2;
        }
        os.bytes_used()
    };
    if used == 0 {
        return 3;
    }

    let mut probe = Probe::default();
    if IStream::new().feed(&buf[..used], &mut probe).is_err() {
        return 4;
    }
    if probe.a != 42 {
        return 5;
    }
    if probe.b != -7 {
        return 6;
    }
    0
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
