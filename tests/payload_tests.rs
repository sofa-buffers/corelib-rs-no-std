//! [`PayloadAcc`] — chunk reassembly for streamed `string` / `blob` payloads
//! (generator #345, CORELIB_PLAN §7).
//!
//! These are the obligations the shared vectors cannot express. A vector fixes
//! the *bytes*, and every accumulator that produces the right field passes it
//! whatever it does at a chunk boundary; the properties that matter here are
//! about the boundary itself — that the assembled value does not depend on where
//! the input was cut, that a payload is handed back once, and that storage this
//! port cannot grow reports its limit instead of truncating. The known gap is
//! exactly here: the shared `invalid_utf8` vectors never reach a chunk with
//! `offset >= total`.
//!
//! A `string`/`blob` payload only exists with the `fixlen` wire type, so the
//! suite is gated on that feature (on by default); a build without it compiles
//! the accumulator away and this file with it.

#![cfg(feature = "fixlen")]

use sofab::{Error, FixlenType, IStream, Id, OStream, PayloadAcc, Visitor};

/// Feed `payload` in fixed-size pieces of `step` bytes and return what the
/// accumulator finally yielded — the way generated code materializes a field,
/// just collected instead of placed.
fn assemble<const N: usize>(payload: &[u8], step: usize) -> Option<Vec<u8>> {
    let mut acc = PayloadAcc::<N>::new();
    let mut done = None;
    let mut offset = 0;
    while offset < payload.len() {
        let end = (offset + step).min(payload.len());
        match acc.feed(payload.len(), offset, &payload[offset..end]) {
            Ok(Some(bytes)) => {
                assert!(done.is_none(), "payload handed back twice");
                done = Some(bytes.to_vec());
            }
            Ok(None) => {}
            Err(e) => panic!("unexpected {e:?} at offset {offset}"),
        }
        offset = end;
    }
    done
}

#[test]
fn whole_payload_in_one_chunk_is_handed_straight_back() {
    let mut acc = PayloadAcc::<32>::new();
    assert_eq!(acc.feed(5, 0, b"sofab"), Ok(Some(&b"sofab"[..])));
    // The point of the fast path: nothing was copied on the way.
    assert_eq!(acc.buffered(), 0);
}

#[test]
fn every_split_of_a_payload_yields_the_same_bytes() {
    // The obligation the shared vectors cannot express: the assembled value must
    // not depend on where the chunk boundaries fell. `step` walks every fixed
    // split, and the explicit two-part loop walks every single cut point 1..n —
    // including the ones inside the multi-byte sequences, where a per-chunk
    // UTF-8 check would go wrong.
    let payload = "sofäbuffers — ünicode ✓".as_bytes();
    for step in 1..=payload.len() + 2 {
        assert_eq!(
            assemble::<64>(payload, step).as_deref(),
            Some(payload),
            "split into {step}-byte chunks"
        );
    }
    for cut in 1..payload.len() {
        let mut acc = PayloadAcc::<64>::new();
        assert_eq!(acc.feed(payload.len(), 0, &payload[..cut]), Ok(None));
        assert_eq!(acc.buffered(), cut);
        assert_eq!(
            acc.feed(payload.len(), cut, &payload[cut..]),
            Ok(Some(payload)),
            "cut at {cut}"
        );
    }
}

#[test]
fn empty_payload_completes_immediately() {
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(0, 0, b""), Ok(Some(&b""[..])));
    assert_eq!(acc.buffered(), 0);
}

#[test]
fn incomplete_payload_yields_nothing() {
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(4, 0, b"so"), Ok(None));
    assert_eq!(acc.buffered(), 2);
    assert_eq!(acc.feed(4, 2, b"f"), Ok(None));
    assert_eq!(acc.buffered(), 3);
}

#[test]
fn a_new_payload_drops_what_the_previous_one_left() {
    let mut acc = PayloadAcc::<16>::new();
    assert_eq!(acc.feed(16, 0, b"abandoned"), Ok(None));
    // No reset in between: `offset == 0` is the reset.
    assert_eq!(acc.feed(4, 0, b"so"), Ok(None));
    assert_eq!(acc.buffered(), 2);
    assert_eq!(acc.feed(4, 2, b"fa"), Ok(Some(&b"sofa"[..])));
}

#[test]
fn a_completed_payload_is_not_handed_back_twice() {
    // A chunk with `offset >= total`: the decoder never emits one, and the
    // shared `invalid_utf8` vectors never reach the case, so this is the
    // boundary that would otherwise go untested. A second, shorter copy here
    // would truncate a field that was already correct.
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(4, 0, b"so"), Ok(None));
    assert_eq!(acc.feed(4, 2, b"fa"), Ok(Some(&b"sofa"[..])));
    assert_eq!(acc.feed(4, 4, b""), Ok(None));
    assert_eq!(acc.feed(4, 4, b"more"), Ok(None));

    // Same for the fast path, which hands the payload back without buffering.
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(4, 0, b"sofa"), Ok(Some(&b"sofa"[..])));
    assert_eq!(acc.feed(4, 4, b""), Ok(None));
    assert_eq!(acc.feed(4, 4, b"more"), Ok(None));
}

#[test]
fn a_chunk_that_does_not_continue_this_payload_is_refused() {
    // `offset` must be where the accumulator stands. A chunk that skips ahead
    // belongs to a payload this accumulator is not assembling, and splicing it
    // in would fabricate a field out of two unrelated halves.
    let mut acc = PayloadAcc::<16>::new();
    assert_eq!(acc.feed(6, 0, b"so"), Ok(None));
    assert_eq!(acc.feed(6, 4, b"ab"), Ok(None), "gap at offset 2..4");
    assert_eq!(acc.buffered(), 2, "the stray chunk was not taken");
    // The payload still completes from its own bytes.
    assert_eq!(acc.feed(6, 2, b"fabs"), Ok(Some(&b"sofabs"[..])));
}

#[test]
fn over_delivery_does_not_widen_the_field() {
    // Both paths cut at `total`: a source that hands over more than was
    // announced must not be able to lengthen the value.
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(3, 0, b"sofabuffers"), Ok(Some(&b"sof"[..])));

    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(3, 0, b"so"), Ok(None));
    assert_eq!(acc.feed(3, 2, b"fabuffers"), Ok(Some(&b"sof"[..])));
    // Only what the field announced was ever written, so an over-delivering
    // source cannot overrun storage sized for `total` either.
    assert_eq!(acc.buffered(), 3);
}

#[test]
fn a_shrinking_total_does_not_wrap_the_arithmetic() {
    // Not something the decoder does — but `total` is an argument, and an
    // accumulator that computes `total - buffered` on a caller that changed its
    // mind would underflow and panic on the spot. Firmware does not get to
    // panic; the answer is defined instead.
    let mut acc = PayloadAcc::<16>::new();
    assert_eq!(acc.feed(9, 0, b"sofab"), Ok(None));
    assert_eq!(acc.feed(3, 5, b"!"), Ok(Some(&b"sof"[..])));
}

#[test]
fn reset_drops_a_partial_payload() {
    let mut acc = PayloadAcc::<16>::new();
    assert_eq!(acc.feed(8, 0, b"partial"), Ok(None));
    acc.reset();
    assert_eq!(acc.buffered(), 0);
    // The dropped bytes are gone rather than merely hidden: a continuation of
    // the abandoned payload cannot complete it out of stale state.
    assert_eq!(acc.feed(8, 7, b"!"), Ok(None));
    assert_eq!(acc.buffered(), 0);
}

#[test]
fn the_accumulator_is_reused_across_payloads() {
    // One accumulator per decoder, not one per field: a second split payload
    // starts from the same storage and carries nothing over from the first.
    let mut acc = PayloadAcc::<8>::new();
    assert_eq!(acc.feed(6, 0, b"sof"), Ok(None));
    assert_eq!(acc.feed(6, 3, b"abs"), Ok(Some(&b"sofabs"[..])));
    assert_eq!(acc.feed(6, 0, b"buf"), Ok(None));
    assert_eq!(acc.feed(6, 3, b"fer"), Ok(Some(&b"buffer"[..])));
}

#[test]
fn a_contiguous_payload_may_exceed_the_storage() {
    // The fast path never touches the buffer, so what it can hand back is bound
    // by the input, not by `N`. This is why the capacity check sits after it: a
    // 1 KiB field decoded from one contiguous slice costs an accumulator of
    // zero bytes.
    let big = vec![0xA5u8; 1024];
    let mut acc = PayloadAcc::<0>::new();
    assert_eq!(acc.feed(big.len(), 0, &big), Ok(Some(&big[..])));
    assert_eq!(acc.capacity(), 0);
    assert_eq!(acc.buffered(), 0);
}

#[test]
fn a_split_payload_larger_than_the_storage_is_refused() {
    // The boundary a fixed buffer has and a `Vec` does not. Truncating the field
    // would be a silent data loss; returning "not complete yet" forever would
    // hide it as a hang.
    let mut acc = PayloadAcc::<4>::new();
    assert_eq!(acc.feed(5, 0, b"sof"), Err(Error::BufferFull));
    assert_eq!(acc.buffered(), 0, "refused before a byte was copied");
    // Every further chunk of the same payload says the same thing, so a caller
    // that reports the first error still gets a consistent answer if it does not.
    assert_eq!(acc.feed(5, 3, b"ab"), Err(Error::BufferFull));
    assert_eq!(
        acc.feed(5, 0, b"sofab"),
        Ok(Some(&b"sofab"[..])),
        "contiguous"
    );

    // And the next payload that does fit is unaffected.
    let mut acc = PayloadAcc::<4>::new();
    assert_eq!(acc.feed(5, 0, b"sof"), Err(Error::BufferFull));
    assert_eq!(acc.feed(4, 0, b"so"), Ok(None));
    assert_eq!(acc.feed(4, 2, b"fa"), Ok(Some(&b"sofa"[..])));
}

#[test]
fn an_announced_total_costs_only_the_bytes_that_arrive() {
    // The eager-allocation guard in the shape this port can have it: `total` is
    // decoded input, so a hostile message announcing 1 GiB and then sending
    // three bytes must not move a byte of storage — the announcement is refused
    // against the capacity that exists, not honoured against the one claimed.
    let mut acc = PayloadAcc::<64>::new();
    assert_eq!(acc.feed(1 << 30, 0, b"three"), Err(Error::BufferFull));
    assert_eq!(acc.buffered(), 0);
}

#[test]
fn capacity_reports_the_reassembly_bound() {
    assert_eq!(PayloadAcc::<0>::new().capacity(), 0);
    assert_eq!(PayloadAcc::<143>::new().capacity(), 143);
}

#[test]
fn default_is_an_empty_accumulator() {
    let mut acc = PayloadAcc::<8>::default();
    assert_eq!(acc.buffered(), 0);
    assert_eq!(acc.feed(2, 0, b"ok"), Ok(Some(&b"ok"[..])));
}

#[test]
fn an_accumulator_can_be_taken_and_put_back() {
    // How generated code holds one: the accumulator lives in the decoder struct
    // between `feed` calls and is moved into the visitor for the duration of
    // one, so a payload split across two calls to the *decoder* survives.
    let mut held = PayloadAcc::<8>::new();
    let mut moved = core::mem::take(&mut held);
    assert_eq!(moved.feed(4, 0, b"so"), Ok(None));
    held = moved;
    let mut moved = core::mem::take(&mut held);
    assert_eq!(moved.feed(4, 2, b"fa"), Ok(Some(&b"sofa"[..])));
}

#[test]
fn debug_reports_the_state_not_the_buffer() {
    let mut acc = PayloadAcc::<143>::new();
    assert_eq!(acc.feed(6, 0, b"sof"), Ok(None));
    let shown = format!("{acc:?}");
    assert!(shown.contains("buffered: 3"), "{shown}");
    assert!(shown.contains("capacity: 143"), "{shown}");
}

// --- the real thing: a visitor that decodes with it -------------------------

/// A [`Visitor`] in the shape generated code has: one accumulator for the whole
/// message, the payload materialized only once it is whole, and a strict
/// `from_utf8` on the assembled bytes (never per chunk).
#[derive(Default)]
struct Fields {
    text: String,
    data: Vec<u8>,
    invalid: bool,
    overflow: bool,
    acc: PayloadAcc<32>,
}

impl Visitor for Fields {
    fn string(&mut self, _id: Id, total: usize, offset: usize, chunk: &[u8]) {
        match self.acc.feed(total, offset, chunk) {
            Ok(Some(bytes)) => match core::str::from_utf8(bytes) {
                Ok(s) => self.text = s.into(),
                Err(_) => self.invalid = true,
            },
            Ok(None) => {}
            Err(_) => self.overflow = true,
        }
    }
    fn blob(&mut self, _id: Id, total: usize, offset: usize, chunk: &[u8]) {
        match self.acc.feed(total, offset, chunk) {
            Ok(Some(bytes)) => self.data = bytes.to_vec(),
            Ok(None) => {}
            Err(_) => self.overflow = true,
        }
    }
}

/// Encode one string and one blob field into a fresh buffer.
fn encode(text: &str, data: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 128];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_str(1, text).unwrap();
        os.write_blob(2, data).unwrap();
        os.bytes_used()
    };
    buf[..used].to_vec()
}

#[test]
fn a_visitor_built_on_it_decodes_the_same_at_every_chunk_size() {
    // The end-to-end claim: one accumulator serves both payloads of a message,
    // field after field, and the decoded values do not depend on how the wire
    // bytes were cut — including one byte at a time, where every payload is
    // split at every position.
    let text = "sofä ✓";
    let data = b"\x00\x01\x02\xff binary";
    let wire = encode(text, data);

    for step in 1..=wire.len() {
        let mut sink = Fields::default();
        let mut is = IStream::new();
        for piece in wire.chunks(step) {
            let _ = is.feed(piece, &mut sink);
        }
        assert_eq!(sink.text, text, "{step}-byte chunks");
        assert_eq!(sink.data.as_slice(), &data[..], "{step}-byte chunks");
        assert!(!sink.invalid && !sink.overflow, "{step}-byte chunks");
    }
}

#[test]
fn invalid_utf8_is_judged_on_the_assembled_payload() {
    // A lone continuation byte cut off from its lead byte: judged per chunk,
    // each half is "just bytes" and the field slips through. The verdict has to
    // land on the whole payload, whatever the chunking — which is what the
    // accumulator is for.
    let mut wire = Vec::new();
    {
        let mut buf = [0u8; 32];
        let used = {
            let mut os = OStream::new(&mut buf);
            // Not reachable through `write_str` (strict by construction), so it
            // goes out as a blob and is retagged below as the string subtype the
            // decoder will see.
            os.write_blob(1, &[0xE2, 0x9C, 0x93, 0xE2, 0x9C]).unwrap();
            os.bytes_used()
        };
        wire.extend_from_slice(&buf[..used]);
    }
    // Flip the fixlen subtype nibble from blob (0x3) to string (0x2).
    wire[1] = (wire[1] & !0x7) | (FixlenType::Str as u8);

    for step in 1..=wire.len() {
        let mut sink = Fields::default();
        let mut is = IStream::new();
        for piece in wire.chunks(step) {
            let _ = is.feed(piece, &mut sink);
        }
        assert!(
            sink.invalid,
            "truncated sequence accepted at {step}-byte chunks"
        );
        assert!(sink.text.is_empty());
    }
}
