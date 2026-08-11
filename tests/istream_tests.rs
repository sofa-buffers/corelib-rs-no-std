//! Decoder tests. Inputs are the exact encoded byte vectors from the C
//! reference suite; we assert the decoded events.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use common::{decode, push_varint, Event, Recorder};
use sofab::{ArrayKind, Error, IStream, Unsigned};

#[test]
fn decode_unsigned() {
    assert_eq!(decode(&[0x00, 0x80, 0x01]), [Event::Unsigned(0, 128)]);
    assert_eq!(
        decode(&[0xF8, 0xFF, 0xFF, 0xFF, 0x3F, 0x00]),
        [Event::Unsigned(sofab::ID_MAX, 0)]
    );
    assert_eq!(
        decode(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Unsigned(0, u64::MAX)]
    );
}

#[test]
fn decode_signed() {
    assert_eq!(
        decode(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Signed(0, i64::MIN)]
    );
    assert_eq!(
        decode(&[0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]),
        [Event::Signed(0, i64::MAX)]
    );
}

#[test]
fn decode_fp32() {
    assert_eq!(
        decode(&[0x02, 0x20, 0x56, 0x0E, 0x49, 0x40]),
        [Event::Fp32(0, 3.1415_f32.to_bits())]
    );
}

#[test]
fn decode_fp64() {
    assert_eq!(
        decode(&[0x02, 0x41, 0x00, 0x00, 0x00, 0x60, 0xFB, 0x21, 0x09, 0x40]),
        [Event::Fp64(0, (3.14159265_f32 as f64).to_bits())]
    );
}

#[test]
fn decode_string() {
    assert_eq!(
        decode(&[
            0x02, 0x62, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68, 0x21
        ]),
        [Event::Str(0, b"Hello Couch!".to_vec())]
    );
}

#[test]
fn decode_string_empty() {
    assert_eq!(decode(&[0x02, 0x02]), [Event::Str(0, vec![])]);
}

#[test]
fn decode_blob() {
    assert_eq!(
        decode(&[0x02, 0x2B, 0x01, 0x02, 0x03, 0x04, 0x05]),
        [Event::Blob(0, vec![1, 2, 3, 4, 5])]
    );
}

#[test]
fn decode_blob_empty() {
    assert_eq!(decode(&[0x02, 0x03]), [Event::Blob(0, vec![])]);
}

#[test]
fn decode_array_of_u32() {
    let bytes = [
        0x03, 0x05, 0x01, 0x02, 0x03, 0x80, 0x80, 0x80, 0x80, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F,
    ];
    assert_eq!(
        decode(&bytes),
        [
            Event::ArrayBegin(0, ArrayKind::Unsigned, 5),
            Event::Unsigned(0, 1),
            Event::Unsigned(0, 2),
            Event::Unsigned(0, 3),
            Event::Unsigned(0, 0x8000_0000),
            Event::Unsigned(0, u32::MAX as u64),
        ]
    );
}

#[test]
fn decode_array_of_i32() {
    let bytes = [
        0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0xFE, 0xFF, 0xFF, 0xFF, 0x0F,
    ];
    assert_eq!(
        decode(&bytes),
        [
            Event::ArrayBegin(0, ArrayKind::Signed, 5),
            Event::Signed(0, -1),
            Event::Signed(0, -2),
            Event::Signed(0, -3),
            Event::Signed(0, i32::MIN as i64),
            Event::Signed(0, i32::MAX as i64),
        ]
    );
}

#[test]
fn decode_array_of_fp32() {
    let bytes = [
        0x05, 0x05, 0x20, 0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x40, 0x40,
        0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0x7F, 0x7F,
    ];
    let want = [1.0_f32, 2.0, 3.0, -f32::MAX, f32::MAX];
    let mut expected = vec![Event::ArrayBegin(0, ArrayKind::Fp32, 5)];
    expected.extend(want.iter().map(|f| Event::Fp32(0, f.to_bits())));
    assert_eq!(decode(&bytes), expected);
}

#[test]
fn decode_nested_sequence() {
    let bytes = [0x00, 0x2A, 0x0E, 0x00, 0x2A, 0x11, 0x53, 0x07, 0x11, 0x53];
    assert_eq!(
        decode(&bytes),
        [
            Event::Unsigned(0, 42),
            Event::SequenceBegin(1),
            Event::Unsigned(0, 42),
            Event::Signed(2, -42),
            Event::SequenceEnd,
            Event::Signed(2, -42),
        ]
    );
}

// --- streaming: identical result regardless of how bytes are chunked --------

#[test]
fn streaming_chunked_feed_matches_oneshot() {
    // A message with a varint that spans a chunk boundary and a string that
    // spans several boundaries.
    let msg = [
        0x00, 0x80, 0x01, // unsigned id0 = 128 (varint split below)
        0x02, 0x62, // string id0, len 12
        0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68,
        0x21, // "Hello Couch!"
    ];
    let oneshot = decode(&msg);

    // Feed one byte at a time. A chunk that ends mid-field reports INCOMPLETE
    // (§7) — that is the streaming "feed me more" signal, not an error — and the
    // final byte (completing the string) returns COMPLETE.
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    let mut last = Ok(());
    for b in msg {
        last = is.feed(&[b], &mut rec);
        assert!(matches!(last, Ok(()) | Err(Error::Incomplete)));
    }
    assert_eq!(last, Ok(()));
    assert_eq!(rec.events, oneshot);

    // Feed in awkward 3-byte chunks.
    let mut rec2 = Recorder::new();
    let mut is2 = IStream::new();
    let mut last2 = Ok(());
    for chunk in msg.chunks(3) {
        last2 = is2.feed(chunk, &mut rec2);
        assert!(matches!(last2, Ok(()) | Err(Error::Incomplete)));
    }
    assert_eq!(last2, Ok(()));
    assert_eq!(rec2.events, oneshot);
}

// --- error cases ------------------------------------------------------------

// --- zero-count arrays (§4.7/§4.8) ------------------------------------------

#[test]
fn decode_empty_unsigned_array() {
    // §4.7: `[ header ][ count = 0 ]` decodes to a single array_begin(.., 0).
    assert_eq!(
        decode(&[0x03, 0x00]),
        [Event::ArrayBegin(0, ArrayKind::Unsigned, 0)]
    );
}

#[test]
fn decode_empty_signed_array() {
    assert_eq!(
        decode(&[0x04, 0x00]),
        [Event::ArrayBegin(0, ArrayKind::Signed, 0)]
    );
}

#[test]
fn decode_empty_fixlen_array_reads_word() {
    // §4.8: a zero-count fixlen array still carries its `fixlen_word` (here 0x20
    // = fp32); the decoder must consume it (no payload) and resume cleanly on
    // the next field (here `id0 = 42`).
    assert_eq!(
        decode(&[0x05, 0x00, 0x20, 0x00, 0x2A]),
        [
            Event::ArrayBegin(0, ArrayKind::Fp32, 0),
            Event::Unsigned(0, 42),
        ]
    );
}

// --- nesting depth (§4.9/§6.2, MAX_DEPTH = 255) -----------------------------

#[test]
fn nesting_at_max_depth_is_accepted() {
    // 255 sequence-start markers (id 0 -> byte 0x06): exactly MAX_DEPTH levels.
    // These are valid — *not* rejected as InvalidMsg — but the 255 sequences are
    // still open, so the outcome is INCOMPLETE, not COMPLETE (§7). The contrast
    // with `nesting_past_max_depth_is_invalid` is the point: at MAX_DEPTH the
    // input is a well-formed prefix; one deeper is malformed.
    let starts = [0x06u8; 255];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&starts, &mut rec), Err(Error::Incomplete));
}

#[test]
fn nesting_past_max_depth_is_invalid() {
    // One level deeper than MAX_DEPTH must be rejected.
    let starts = [0x06u8; 256];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&starts, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn varint_overflow_is_invalid() {
    // 11 continuation bytes overflow the 64-bit value type.
    let bytes = [
        0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    ];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn overlong_varint_final_high_bits_is_invalid() {
    // §4.1/§6.3: a 10-byte varint whose 10th byte sets a bit above bit 63 is a
    // >64-bit overflow and must be rejected as INVALID, not silently truncated.
    // Reproducer F-0016: a u64 field (id 6, header 0x30) carrying the overlong
    // varint. The 65th-bit form (…02) and the bits-64..69 form (…7f) both spill.
    for terminator in [0x02u8, 0x7f] {
        let bytes = [
            0x30, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, terminator,
        ];
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        assert_eq!(
            is.feed(&bytes, &mut rec),
            Err(Error::InvalidMsg),
            "overlong varint terminator {terminator:#04x} must be INVALID",
        );
    }
}

#[test]
fn max_u64_varint_is_accepted() {
    // Control (F-0016): the valid maximum 2^64-1 (…01 in the 10th byte) must
    // still decode — the overlong-varint guard must not reject it.
    let bytes = [
        0x30, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
    ];
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Ok(()));
    assert_eq!(rec.events, [Event::Unsigned(6, u64::MAX)]);
}

#[test]
fn last_varint_byte_accepts_exactly_the_bits_that_fit() {
    // §4.1/§6.3 at its exact boundary, for whichever value width this build
    // has. `shift` only moves in steps of 7 from 0, so *both* ways a varint can
    // overrun the width — one more continuation byte, or a payload bit above
    // the width — can only show up in a single byte position: the last one that
    // can still carry payload (the 10th byte of a 64-bit varint, the 5th of a
    // 32-bit one). The decoder folds them into the one test that fires there,
    // so walk every one of the 256 possible bytes in that position and pin
    // accept/reject for each — the fold must not widen or narrow what is legal.
    let width = Unsigned::BITS;
    let groups = width / 7; // full 7-bit groups before the last byte
    let carried = groups * 7; // payload bits they hold
    let room = width - carried; // payload bits left for the last byte

    for terminator in 0u16..=0xFF {
        let terminator = terminator as u8;
        let mut bytes = vec![0x30]; // VARINT_UNSIGNED, id 6
        bytes.extend(std::iter::repeat(0xFFu8).take(groups as usize)); // 0x80 | 0x7f
        bytes.push(terminator);

        let mut rec = Recorder::new();
        let mut is = IStream::new();
        let outcome = is.feed(&bytes, &mut rec);

        // Legal iff it terminates the varint and sets no bit above the width.
        if terminator & 0x80 == 0 && u32::from(terminator) >> room == 0 {
            let expected = (!(0 as Unsigned) >> room) | ((terminator as Unsigned) << carried);
            assert_eq!(
                outcome,
                Ok(()),
                "terminator {terminator:#04x} fits in {width} bits and must decode",
            );
            assert_eq!(rec.events, [Event::Unsigned(6, expected)]);
        } else {
            assert_eq!(
                outcome,
                Err(Error::InvalidMsg),
                "terminator {terminator:#04x} overruns {width} bits and must be INVALID",
            );
        }
    }
}

#[test]
fn dangling_sequence_end_is_invalid() {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&[0x07], &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn id_above_max_is_invalid() {
    // Craft a header whose id field is ID_MAX + 1, type unsigned.
    let header = (sofab::ID_MAX as u64 + 1) << 3; // type tag 0 = unsigned
    let mut bytes = Vec::new();
    push_varint(&mut bytes, header);
    bytes.push(0x00); // value
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn fp32_with_wrong_length_is_invalid() {
    // FIXLEN, subtype FP32 (0), but length 2 instead of 4.
    let bytes = [0x02, 2 << 3, 0xAA, 0xBB]; // len 2, subtype FP32 (tag 0)
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn reserved_fixlen_subtype_is_invalid() {
    // §7 malformed(reserved subtype). FIXLEN header (0x02), then a fixlen word
    // whose low 3 bits are a reserved subtype tag 0x4 (len 0). `FixlenType::from_raw`
    // rejects tags 0x4–0x7, so the decode is InvalidMsg (not a truncation).
    let bytes = [0x02, 0x04]; // fixlen word: subtype 0x4 (reserved), length 0
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn oversized_array_count_is_invalid() {
    // §7 malformed(oversized count). VARINTARRAY_UNSIGNED header (0x03), then a
    // count varint of 2^31 — one past ARRAY_MAX (2^31 − 1). Rejected as InvalidMsg.
    let mut bytes = vec![0x03];
    push_varint(&mut bytes, 1u64 << 31); // count = 2^31 > ARRAY_MAX
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

#[test]
fn oversized_fixlen_length_is_invalid() {
    // §7 malformed(oversized length). FIXLEN header (0x02), then a fixlen word
    // encoding length 2^31 (one past ARRAY_MAX) with subtype FP32 (tag 0):
    // word = (2^31 << 3) | 0. Rejected as InvalidMsg.
    let mut bytes = vec![0x02];
    push_varint(&mut bytes, (1u64 << 31) << 3); // length 2^31, subtype 0
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&bytes, &mut rec), Err(Error::InvalidMsg));
}

// --- three-valued decode outcome (§7): COMPLETE / INCOMPLETE / INVALID -------
//
// INCOMPLETE (bytes end inside a field, or with an open sequence) is a distinct,
// first-class outcome — never silently folded into COMPLETE (`Ok`) nor promoted
// to INVALID. `outcome` returns the raw status of a one-shot feed.

/// Feed `bytes` in one shot and return the raw three-valued decode outcome.
fn outcome(bytes: &[u8]) -> Result<(), Error> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(bytes, &mut rec)
}

#[test]
fn lone_continuation_byte_is_incomplete() {
    // A lone 0x80 is a well-formed *prefix* of a varint (continuation bit set,
    // no terminator): the caller may still complete it. INCOMPLETE, not INVALID
    // (§7, called out by name in the spec).
    assert_eq!(outcome(&[0x80]), Err(Error::Incomplete));
}

#[test]
fn oversized_varint_is_invalid_not_incomplete() {
    // 11 continuation bytes overflow the 64-bit value type: malformed
    // regardless of what follows, so INVALID — must NOT be reported as
    // INCOMPLETE even though it, too, "ends mid-varint".
    let bytes = [
        0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
    ];
    assert_eq!(outcome(&bytes), Err(Error::InvalidMsg));
}

#[test]
fn complete_message_is_ok() {
    // Header + full value, ending exactly at a field boundary: COMPLETE.
    assert_eq!(outcome(&[0x00, 0x80, 0x01]), Ok(())); // unsigned id0 = 128
}

#[test]
fn header_without_value_is_incomplete() {
    // Header announces an unsigned value but no value byte arrives: mid-field.
    assert_eq!(outcome(&[0x00]), Err(Error::Incomplete));
}

#[test]
fn truncated_varint_value_is_incomplete() {
    // Header + a partial multi-byte value (continuation set, no terminator).
    assert_eq!(outcome(&[0x00, 0x80]), Err(Error::Incomplete));
}

#[test]
fn truncated_fixlen_payload_is_incomplete() {
    // fp32 header declares a 4-byte payload; only 2 bytes arrive.
    assert_eq!(outcome(&[0x02, 0x20, 0x00, 0x00]), Err(Error::Incomplete));
}

#[test]
fn truncated_string_payload_is_incomplete() {
    // string id0 len 12; only 2 of the 12 payload bytes are delivered.
    assert_eq!(outcome(&[0x02, 0x62, 0x48, 0x65]), Err(Error::Incomplete));
}

#[test]
fn open_sequence_is_incomplete() {
    // A sequence-start with no matching sequence-end: valid so far, not closed.
    assert_eq!(outcome(&[0x06]), Err(Error::Incomplete));
}

#[test]
fn truncated_array_element_is_incomplete() {
    // Array of 2 unsigned; header + count + first element, second missing.
    assert_eq!(outcome(&[0x03, 0x02, 0x01]), Err(Error::Incomplete));
}

#[test]
fn empty_input_is_complete() {
    // Zero bytes end (trivially) exactly at a field boundary.
    assert_eq!(outcome(&[]), Ok(()));
}

// --- §7.2 item 5: the id ceiling binds *every* header ------------------------

#[test]
fn oversized_id_on_a_sequence_end_header_is_invalid() {
    // §6.2 admits no exception: `ID_MAX` bounds the id of every field header,
    // the value-bearing ones and the **sequence-end** marker alike. That a
    // sequence end's id is discarded rather than used (§4.9) does not exempt it
    // — the ceiling is stated over headers, not over headers whose id a decoder
    // happens to consult.
    //
    // The spec calls this case out by name because an implementation that
    // validates the id only in the branches that *use* it passes
    // `id_above_max_is_invalid` above and fails exactly here.
    let mut bytes = vec![0x06]; // sequence start, id 0 — so the end is balanced
    push_varint(&mut bytes, ((sofab::ID_MAX as u64 + 1) << 3) | 0x07);
    assert_eq!(outcome(&bytes), Err(Error::InvalidMsg));
}

// --- §7.2 item 5b: tolerance — non-canonical but well-formed -----------------
//
// The mirror of the malformed-input tests: input a decoder must NOT reject.
// These are the cases a majority-vote conformance check cannot catch, since
// every implementation may be uniformly too strict. Each one must decode to the
// value it denotes **and** re-encode canonically (§4.1, §4.9).

/// Re-encode a recorded event stream, so "decodes to the value it denotes and
/// re-encodes canonically" is asserted rather than argued. Covers only the
/// event kinds the tolerance tests below produce.
///
/// Sequences close with `write_sequence_end_keep`: these frames were *on the
/// wire*, so the frame itself carries information and must survive the
/// round-trip. `write_sequence_end` would drop a contentless one, which is the
/// MESSAGE_SPEC §2 omission of an all-default sequence *field* — a schema
/// property, not a property of these bytes.
fn reencode(events: &[Event]) -> Vec<u8> {
    let mut buf = [0u8; 128];
    let used = {
        let mut os = sofab::OStream::new(&mut buf);
        let mut i = 0;
        while i < events.len() {
            match &events[i] {
                Event::Unsigned(id, v) => os.write_unsigned(*id, *v).unwrap(),
                Event::Signed(id, v) => os.write_signed(*id, *v).unwrap(),
                Event::Fp32(id, bits) => os.write_fp32(*id, f32::from_bits(*bits)).unwrap(),
                Event::SequenceBegin(id) => os.write_sequence_begin_lazy(*id).unwrap(),
                Event::SequenceEnd => os.write_sequence_end_keep().unwrap(),
                Event::ArrayBegin(id, ArrayKind::Unsigned, count) => {
                    // The elements follow as their own events; take them here so
                    // the array is written as one field.
                    let elems: Vec<u64> = events[i + 1..i + 1 + count]
                        .iter()
                        .map(|e| match e {
                            Event::Unsigned(_, v) => *v,
                            other => panic!("unsigned array element expected, got {other:?}"),
                        })
                        .collect();
                    os.write_array_unsigned(*id, &elems).unwrap();
                    i += count;
                }
                other => panic!("reencode: unsupported event {other:?}"),
            }
            i += 1;
        }
        os.bytes_used()
    };
    buf[..used].to_vec()
}

/// Assert that `noncanonical` decodes exactly like `canonical` and re-encodes
/// to it — never `INVALID`.
fn tolerated(noncanonical: &[u8], canonical: &[u8]) {
    assert_eq!(
        outcome(noncanonical),
        Ok(()),
        "non-canonical but well-formed input must not be rejected",
    );
    let events = decode(noncanonical);
    assert_eq!(events, decode(canonical), "must decode to the same value");
    assert_eq!(reencode(&events), canonical, "must re-encode canonically");
}

#[test]
fn a_non_minimal_field_header_is_tolerated() {
    // `0x80 0x00` is id 0 / type 0 spelled in two bytes. §4.1: minimality is
    // required on encode, tolerated on decode.
    tolerated(&[0x80, 0x00, 0x00], &[0x00, 0x00]);
}

#[test]
fn a_non_minimal_fixlen_word_is_tolerated() {
    // fp32, length 4, subtype 0 — word `0x20` spelled as `0xA0 0x00`.
    let payload = [0x00, 0x00, 0x80, 0x3F]; // 1.0f32, little-endian
    let mut noncanonical = vec![0x02, 0xA0, 0x00];
    noncanonical.extend_from_slice(&payload);
    let mut canonical = vec![0x02, 0x20];
    canonical.extend_from_slice(&payload);
    tolerated(&noncanonical, &canonical);
}

#[test]
fn a_non_minimal_element_count_is_tolerated() {
    // Unsigned array, count 1 spelled `0x81 0x00`, one element = 42.
    tolerated(&[0x03, 0x81, 0x00, 0x2A], &[0x03, 0x01, 0x2A]);
}

#[test]
fn a_sequence_end_id_that_is_non_zero_but_in_range_is_tolerated() {
    // §4.9: the marker closes the innermost open sequence **whatever the id
    // says**. `0x0F` is id 1 / type 7 — an ordinary sequence end that must
    // re-emit as `0x07`. Rejecting it is the "uniformly too strict" failure
    // this item exists to catch.
    tolerated(&[0x06, 0x0F], &[0x06, 0x07]);

    // The same id at the top of its range, to pin that the tolerance is the
    // whole range and not a special case for small ids.
    let mut bytes = vec![0x06];
    push_varint(&mut bytes, ((sofab::ID_MAX as u64) << 3) | 0x07);
    tolerated(&bytes, &[0x06, 0x07]);
}

#[test]
fn a_non_minimally_spelled_sequence_end_is_tolerated() {
    // `0x87 0x00` — id 0, type 7, two bytes. Both tolerances at once.
    tolerated(&[0x06, 0x87, 0x00], &[0x06, 0x07]);
}

// --- §7.2 item 6: no partial evaluation of a varint --------------------------

#[test]
fn a_fixlen_word_cut_after_a_reserved_subtype_byte_is_incomplete() {
    // §4.1: a varint has **no value** until its final byte. The low 3 bits of
    // any varint are settled by its first byte — so after `0x84` the subtype
    // `0x4` (reserved) is already arithmetically fixed and no continuation byte
    // can change it. A decoder MUST NOT act on it: the word is unfinished, so
    // the message is INCOMPLETE, not INVALID.
    //
    // Nothing else in the malformed/truncation suites exercises this rule — a
    // dangling `0x80` carries no settled sub-field to peek at, and
    // `reserved_fixlen_subtype_is_invalid` above feeds the *complete* word.
    assert_eq!(outcome(&[0x02, 0x84]), Err(Error::Incomplete));
    assert_eq!(outcome(&[0x02, 0x8C]), Err(Error::Incomplete)); // subtype 0x5
    assert_eq!(outcome(&[0x02, 0xB4]), Err(Error::Incomplete)); // subtype 0x4, longer

    // Completing the same word settles it — and *then* the reserved subtype is
    // INVALID. The two outcomes differ only in where the bytes stop, which is
    // the whole point.
    assert_eq!(outcome(&[0x02, 0x84, 0x00]), Err(Error::InvalidMsg));

    // The same for a fixlen array's second word, reached after the count.
    assert_eq!(outcome(&[0x05, 0x01, 0x84]), Err(Error::Incomplete));
}

// --- §7.2 item 4: a fed chunk is borrowed only for the duration of `feed` ----

#[test]
fn a_fed_chunk_may_be_overwritten_the_moment_feed_returns() {
    // §6 chunk lifetime: once `feed` returns, the caller may reuse, overwrite or
    // free that memory and the decoded message MUST NOT be affected. Every chunk
    // is scrubbed with a fill byte immediately after the call, and — because the
    // same scratch buffer is reused for all of them — a decoder that kept a
    // slice into a fed chunk reads back the fill pattern. Nothing else in the
    // suite would notice.
    let text = "a string long enough to straddle several chunk boundaries";
    let blob: Vec<u8> = (0..64u16).map(|i| i as u8).collect();

    let mut buf = [0u8; 256];
    let used = {
        let mut os = sofab::OStream::new(&mut buf);
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, text).unwrap();
        os.write_blob(3, &blob).unwrap();
        os.write_signed(4, -7).unwrap();
        os.bytes_used()
    };
    let wire = buf[..used].to_vec();
    let want = decode(&wire);

    for chunk_size in [1usize, 5, 7, 16] {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        let mut scratch = [0u8; 16];
        for piece in wire.chunks(chunk_size) {
            let n = piece.len();
            scratch[..n].copy_from_slice(piece);
            match is.feed(&scratch[..n], &mut rec) {
                Ok(()) | Err(Error::Incomplete) => {}
                Err(e) => panic!("chunked decode (chunk={chunk_size}): {e:?}"),
            }
            scratch.fill(0xAA); // the caller reuses the buffer straight away
        }
        assert_eq!(rec.events, want, "chunk size {chunk_size}");
    }
}

// --- §5.2: INVALID is terminal ----------------------------------------------
//
// The decode-outcome table's last column says it for `INVALID`: "can more bytes
// change it? — no, terminal". Once a decoder has determined that the bytes it
// consumed are malformed *regardless of what follows*, no continuation can undo
// that, so every later `feed` must keep reporting `INVALID` — and must not push
// further fields to the visitor. Without that latch the verdict depends on where
// the chunk boundary falls, which §7.2 item 4 forbids: feeding a malformed
// prefix and a well-formed field in one call reports INVALID, while feeding them
// as two calls reports INVALID and then COMPLETE, delivering a field out of a
// message already proven broken.

/// Every `INVALID` condition of the §5.2 table that this port can be driven
/// into, as (name, malformed prefix). Each one is already asserted INVALID on
/// its own above; here they are the *precondition* of the terminal-ness tests.
fn invalid_prefixes() -> Vec<(&'static str, Vec<u8>)> {
    let mut cases: Vec<(&'static str, Vec<u8>)> = Vec::new();

    // A varint past the 64-bit bound (§4.1).
    cases.push((
        "overlong varint",
        vec![
            0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        ],
    ));

    // An id above ID_MAX (§6.2).
    let mut id_over = Vec::new();
    push_varint(&mut id_over, (sofab::ID_MAX as u64 + 1) << 3);
    cases.push(("id above ID_MAX", id_over));

    // A sequence-end marker with no open sequence (§4.9).
    cases.push(("dangling sequence end", vec![0x07]));

    // Nesting past MAX_DEPTH = 255 (§4.9). Note this one *does* deliver events
    // (255 sequence starts) before it fails.
    cases.push(("nesting past MAX_DEPTH", vec![0x06; 256]));

    // A count above ARRAY_MAX (§6.2).
    let mut count_over = vec![0x03];
    push_varint(&mut count_over, 1u64 << 31);
    cases.push(("array count above ARRAY_MAX", count_over));

    // A fixlen length above its maximum (§6.2).
    let mut len_over = vec![0x02];
    push_varint(&mut len_over, (1u64 << 31) << 3);
    cases.push(("fixlen length above maximum", len_over));

    // A reserved fixlen subtype (§4.6).
    cases.push(("reserved fixlen subtype", vec![0x02, 0x04]));

    // An fp32 whose declared length is not 4 (§4.6).
    cases.push(("fp32 of the wrong width", vec![0x02, 2 << 3]));

    cases
}

/// A well-formed field: unsigned id 1 = 42. Whatever follows a malformed prefix,
/// it must never reach the visitor.
const GOOD_FIELD: [u8; 2] = [0x08, 0x2a];

#[test]
fn invalid_stays_invalid_on_every_later_feed() {
    for (name, prefix) in invalid_prefixes() {
        let mut rec = Recorder::new();
        let mut is = IStream::new();
        assert_eq!(
            is.feed(&prefix, &mut rec),
            Err(Error::InvalidMsg),
            "{name}: the prefix itself must be INVALID",
        );
        let after_error = rec.events.len();

        // A well-formed field, an empty chunk, and a second well-formed field:
        // none of them may resurrect the message.
        assert_eq!(
            is.feed(&GOOD_FIELD, &mut rec),
            Err(Error::InvalidMsg),
            "{name}: a valid field after the error must stay INVALID",
        );
        assert_eq!(
            is.feed(&[], &mut rec),
            Err(Error::InvalidMsg),
            "{name}: an empty feed after the error must stay INVALID",
        );
        assert_eq!(
            is.feed(&GOOD_FIELD, &mut rec),
            Err(Error::InvalidMsg),
            "{name}: still INVALID on the third feed",
        );
        assert_eq!(
            rec.events.len(),
            after_error,
            "{name}: no field may be delivered out of a message proven malformed",
        );
    }
}

#[test]
fn the_chunked_verdict_matches_the_one_shot_verdict_for_malformed_input() {
    // §7.2 item 4: feeding a stream one byte at a time must be indistinguishable
    // from feeding it whole. For malformed input that means the *final* outcome
    // and the delivered events agree — and that no intermediate feed reports
    // anything but INVALID once the malformed byte has been consumed.
    for (name, prefix) in invalid_prefixes() {
        let mut wire = prefix.clone();
        wire.extend_from_slice(&GOOD_FIELD);

        let mut one_shot = Recorder::new();
        let mut is = IStream::new();
        let whole = is.feed(&wire, &mut one_shot);
        assert_eq!(whole, Err(Error::InvalidMsg), "{name}: one-shot");

        let mut chunked = Recorder::new();
        let mut is = IStream::new();
        let mut seen_invalid = false;
        let mut last = Ok(());
        for byte in &wire {
            last = is.feed(&[*byte], &mut chunked);
            if seen_invalid {
                assert_eq!(
                    last,
                    Err(Error::InvalidMsg),
                    "{name}: a feed after the malformed byte reported {last:?}",
                );
            }
            seen_invalid |= last == Err(Error::InvalidMsg);
        }
        assert!(
            seen_invalid,
            "{name}: byte-at-a-time never reported INVALID"
        );
        assert_eq!(last, whole, "{name}: final outcome differs from one-shot");
        assert_eq!(
            chunked.events, one_shot.events,
            "{name}: byte-at-a-time delivered different fields",
        );
    }
}
