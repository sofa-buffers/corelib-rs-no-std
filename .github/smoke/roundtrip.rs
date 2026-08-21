//! Host smoke test for the *packaged* crate, run by `.github/workflows/release.yml`.
//!
//! The repository's own suite proves the working tree is correct. This proves
//! something the suite structurally cannot: that the artifact `cargo package`
//! produced — the exact file set that reaches crates.io — still builds and
//! works when consumed as an ordinary dependency from outside the repository.
//!
//! It stays allocation-free on the corelib's side of the API (the crate has no
//! `alloc`), and touches only what the **default** feature set provides.
//! `baremetal.rs` is the companion that proves the same package links with no
//! host `std` at all.

use sofab::{Error, IStream, Id, OStream, Signed, Unsigned, Visitor};

#[derive(Default)]
struct Probe {
    a: Unsigned,
    b: Signed,
    s: [u8; 8],
    s_len: usize,
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
    fn string(&mut self, id: Id, _total: usize, off: usize, chunk: &[u8]) {
        if id == 3 {
            // Chunked delivery: `off` is where this piece belongs.
            self.s[off..off + chunk.len()].copy_from_slice(chunk);
            self.s_len = off + chunk.len();
        }
    }
}

fn main() {
    // Encode into a caller-owned, fixed-capacity buffer.
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, 42).expect("write field 1");
        os.write_signed(2, -7).expect("write field 2");
        os.write_str(3, "hi").expect("write field 3");
        os.bytes_used()
    };
    let wire = &buf[..used];
    assert!(
        !wire.is_empty(),
        "three non-default fields must produce bytes"
    );

    // Decode it back in one feed.
    let mut probe = Probe::default();
    IStream::new()
        .feed(wire, &mut probe)
        .expect("decode the message just encoded");
    assert_eq!(probe.a, 42, "field 1 round-tripped");
    assert_eq!(probe.b, -7, "field 2 round-tripped");
    assert_eq!(&probe.s[..probe.s_len], b"hi", "field 3 round-tripped");

    // The same message one byte at a time: the decoder must suspend at every
    // boundary and land on the identical value. A cut inside a field reports
    // `Incomplete` — that is the suspend, not a failure — while a cut on a
    // field boundary is `Ok`, because a message ends wherever its last field
    // does. Both are acceptable mid-stream; anything else is a real error.
    let mut chunked = Probe::default();
    let mut is = IStream::new();
    for (i, byte) in wire.iter().enumerate() {
        let outcome = is.feed(core::slice::from_ref(byte), &mut chunked);
        if i + 1 == wire.len() {
            outcome.expect("the final byte completes the message");
        } else {
            assert!(
                matches!(outcome, Ok(()) | Err(Error::Incomplete)),
                "byte {i} must suspend or complete, never fail: {outcome:?}",
            );
        }
    }
    assert_eq!(chunked.a, probe.a);
    assert_eq!(chunked.b, probe.b);
    assert_eq!(&chunked.s[..chunked.s_len], &probe.s[..probe.s_len]);

    println!("smoke ok — {used} bytes: {wire:02x?}");
}
