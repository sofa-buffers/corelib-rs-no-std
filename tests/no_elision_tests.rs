//! A compact scalar array is never shortened (MESSAGE_SPEC §3).
//!
//! `count: N` is a **capacity, not a length**: the count prefix on the wire is
//! the number of elements actually written, and when a field is emitted the
//! encoder writes *every* element it holds — trailing default-valued ones
//! included. Dropping a trailing run of defaults does not produce a compact
//! spelling of the same value, it produces a **different, shorter** value:
//! `[1, 2, 3, 0, 0]` (`M = 5`) and `[1, 2, 3]` (`M = 3`) are distinct arrays and
//! encode differently. There is no fill-to-`N` on the decode side to rebuild
//! what an encoder left out.
//!
//! The tests below pin both halves of that: the encoder keeps the tail on the
//! wire for every array flavour, and the crate exposes no helper that invites a
//! caller to cut it off first.

mod common;

// --- the crate exposes no trailing-elision helper ---------------------------

/// `src/lib.rs` declares every module and re-exports every public item, so any
/// helper offered to callers has to be named here. Earlier releases shipped
/// `trim_tail` / `trim_tail_f32` / `trim_tail_f64`, documented as producing "the
/// canonical wire form" of a fixed-length array by dropping its trailing run of
/// defaults. Under the count-is-capacity rule that is silent data loss, so the
/// helpers are gone; this keeps them from coming back.
#[test]
fn the_crate_root_exposes_no_trailing_elision_helper() {
    const LIB_RS: &str = include_str!("../src/lib.rs");
    for forbidden in ["trim_tail", "mod trim"] {
        assert!(
            !LIB_RS.contains(forbidden),
            "src/lib.rs names `{forbidden}`: trimming an array's trailing defaults \
             changes its value (MESSAGE_SPEC §3), so no such helper belongs in the \
             public surface"
        );
    }
}

// --- the encoder writes every element it is given ---------------------------

#[cfg(feature = "array")]
mod encode {
    use sofab::OStream;

    /// Encode with a fresh stack buffer and return the produced bytes.
    fn encode<F: FnOnce(&mut OStream)>(f: F) -> Vec<u8> {
        let mut buf = [0u8; 128];
        let used = {
            let mut os = OStream::new(&mut buf);
            f(&mut os);
            os.bytes_used()
        };
        buf[..used].to_vec()
    }

    #[test]
    fn an_unsigned_array_keeps_its_trailing_defaults() {
        // Matches the shared vector `array_unsigned_trailing_defaults`: the two
        // zero elements are counted by `M` and written out.
        let full: [u32; 5] = [1, 2, 3, 0, 0];
        assert_eq!(
            encode(|os| os.write_array_unsigned(0, &full).unwrap()),
            [0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0x00]
        );

        // …and the trimmed array is a *different* value, not the same one spelled
        // shorter: different count, different bytes.
        let trimmed: [u32; 3] = [1, 2, 3];
        assert_ne!(
            encode(|os| os.write_array_unsigned(0, &full).unwrap()),
            encode(|os| os.write_array_unsigned(0, &trimmed).unwrap())
        );
    }

    #[test]
    fn a_signed_array_keeps_its_trailing_defaults() {
        let a: [i32; 4] = [-1, 2, 0, 0];
        assert_eq!(
            encode(|os| os.write_array_signed(0, &a).unwrap()),
            [0x04, 0x04, 0x01, 0x04, 0x00, 0x00]
        );
    }

    #[test]
    fn an_all_default_array_keeps_every_element() {
        // Nothing distinguishes this from the zero-count array except `M`, which
        // is exactly why the elements have to be written.
        let a: [u32; 3] = [0, 0, 0];
        assert_eq!(
            encode(|os| os.write_array_unsigned(0, &a).unwrap()),
            [0x03, 0x03, 0x00, 0x00, 0x00]
        );
        let empty: [u32; 0] = [];
        assert_eq!(
            encode(|os| os.write_array_unsigned(0, &empty).unwrap()),
            [0x03, 0x00]
        );
    }

    #[cfg(feature = "fixlen")]
    #[test]
    fn an_fp32_array_keeps_its_trailing_zeros() {
        // Both +0.0 and -0.0 stay: the payload is fixed-width, so the count is
        // the only thing that could have been shortened.
        let a: [f32; 3] = [1.0, 0.0, -0.0];
        assert_eq!(
            encode(|os| os.write_array_fp32(0, &a).unwrap()),
            [
                0x05, 0x03, 0x20, 0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x80
            ]
        );
    }

    #[cfg(feature = "fp64")]
    #[test]
    fn an_fp64_array_keeps_its_trailing_zeros() {
        let a: [f64; 2] = [1.0, 0.0];
        assert_eq!(
            encode(|os| os.write_array_fp64(0, &a).unwrap()),
            [
                0x05, 0x02, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }
}

// --- and the decoder recovers every element ---------------------------------

#[cfg(feature = "array")]
mod roundtrip {
    use crate::common::{Event, Recorder};
    use sofab::{ArrayKind, IStream, OStream, Unsigned};

    #[test]
    fn a_trailing_default_run_survives_the_roundtrip() {
        let a: [u32; 5] = [1, 2, 3, 0, 0];
        let mut buf = [0u8; 64];
        let used = {
            let mut os = OStream::new(&mut buf);
            os.write_array_unsigned(7, &a).unwrap();
            os.bytes_used()
        };

        let mut rec = Recorder::default();
        let mut is = IStream::new();
        is.feed(&buf[..used], &mut rec).unwrap();

        // The array header announces the full length, and every element — the
        // trailing defaults included — comes back out.
        assert_eq!(rec.events[0], Event::ArrayBegin(7, ArrayKind::Unsigned, 5));
        let elements: Vec<Unsigned> = rec.events[1..]
            .iter()
            .map(|e| match e {
                Event::Unsigned(_, v) => *v,
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(elements, [1, 2, 3, 0, 0]);
    }
}
