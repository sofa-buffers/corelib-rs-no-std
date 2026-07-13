//! Decoder tests. Inputs are the exact encoded byte vectors from the C
//! reference suite; we assert the decoded events.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use common::{push_varint, Event, Recorder};
use sofab::{ArrayKind, Error, IStream};

/// Decode `bytes` in one shot and return the recorded events.
fn decode(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("decode failed");
    rec.events
}

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
    let mut expected = vec![Event::ArrayBegin(0, ArrayKind::Fixlen, 5)];
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
            Event::ArrayBegin(0, ArrayKind::Fixlen, 0),
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
