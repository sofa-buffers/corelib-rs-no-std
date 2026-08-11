//! Skip-by-not-handling: what a [`Visitor`] that leaves a callback at its
//! default body does to the decode.
//!
//! Unlike the C decoder there is no "bind a destination" step and no explicit
//! skip call — a consumer simply does not implement the callbacks it does not
//! care about, and those fields are dropped. That is this port's spelling of the
//! skip every other corelib exposes, so it carries the same obligations
//! (CORELIB_PLAN §5.2, MESSAGE_SPEC §7.3): an unread field is **walked**, not
//! jumped over blindly; it leaves the decoder at the next field boundary, so the
//! message stays `COMPLETE`; and it changes nothing about the fields that *are*
//! handled — including the children of a sequence whose `sequence_begin` nobody
//! implemented.
//!
//! Every wire type has to appear for that to mean anything, so the suite needs
//! the full feature set; reduced builds get the same ground covered per feature
//! by `config_tests.rs`.

#![cfg(all(
    feature = "fixlen",
    feature = "fp64",
    feature = "array",
    feature = "sequence"
))]

use sofab::{Error, IStream, Id, OStream, Unsigned, Visitor};

/// A message using **every** wire type: both varint kinds, fp32/fp64, string,
/// blob, all three array kinds, and a nested sequence with children — with
/// unsigned fields interleaved so a mishandled skip shows up as a wrong or
/// missing neighbour rather than only as a bad outcome.
fn message_of_every_wire_type() -> Vec<u8> {
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_unsigned(1, 42).unwrap();
        os.write_signed(2, -7).unwrap();
        os.write_fp32(3, 1.5).unwrap();
        os.write_fp64(4, -2.5).unwrap();
        os.write_str(5, "text").unwrap();
        os.write_blob(6, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        os.write_array_signed(7, &[-1i32, 2, -3]).unwrap();
        os.write_array_fp32(8, &[1.0f32, 2.0]).unwrap();
        os.write_array_fp64(9, &[3.0f64]).unwrap();
        os.write_sequence_begin_lazy(10).unwrap();
        os.write_unsigned(11, 5).unwrap(); // content, so the frame reaches the wire
        os.write_str(12, "inner").unwrap();
        os.write_sequence_end().unwrap();
        os.write_array_unsigned(13, &[7u32, 8]).unwrap();
        os.write_unsigned(14, 99).unwrap();
        os.bytes_used()
    };
    buf[..used].to_vec()
}

/// A consumer that implements **nothing**: every callback stays at the trait's
/// default empty body.
struct Ignore;
impl Visitor for Ignore {}

/// A consumer that implements exactly one callback. Every other field kind falls
/// through to a default body and is dropped.
#[derive(Default)]
struct OnlyUnsigned {
    seen: Vec<(Id, Unsigned)>,
}

impl Visitor for OnlyUnsigned {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.seen.push((id, value));
    }
}

#[test]
fn a_consumer_that_handles_nothing_still_walks_every_wire_type() {
    let msg = message_of_every_wire_type();

    let mut sink = Ignore;
    assert_eq!(
        IStream::new().feed(&msg, &mut sink),
        Ok(()),
        "an unread field must leave the decoder at the next field boundary",
    );

    // Skipping is a walk, not a length jump, so it has to resume at any byte
    // boundary as well.
    let mut is = IStream::new();
    for (i, b) in msg.iter().enumerate() {
        match is.feed(&[*b], &mut sink) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("byte {i}: {e:?}"),
        }
    }
    assert_eq!(is.feed(&[], &mut sink), Ok(()));
}

#[test]
fn dropping_the_other_kinds_leaves_the_handled_fields_exact() {
    // The unsigned fields — including the elements of the unsigned array, which
    // arrive through the same callback under the array's id, and the child of
    // the sequence nobody announced — must be exactly what a full consumer sees,
    // in order.
    let msg = message_of_every_wire_type();

    let mut sink = OnlyUnsigned::default();
    assert_eq!(IStream::new().feed(&msg, &mut sink), Ok(()));
    assert_eq!(sink.seen, [(1, 42), (11, 5), (13, 7), (13, 8), (14, 99)]);

    // Same, one byte per feed: the chunk boundaries fall inside skipped payloads
    // and skipped varints alike.
    let mut chunked = OnlyUnsigned::default();
    let mut is = IStream::new();
    for b in &msg {
        match is.feed(&[*b], &mut chunked) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("chunked: {e:?}"),
        }
    }
    assert_eq!(chunked.seen, sink.seen);
}
