//! The fixlen-array header (CORELIB_PLAN §4.8) — regression suite for F-0042.
//!
//! §4.8 fixes the decode order of a fixlen array (wire type `FIXLENARRAY`):
//!
//! 1. read `element_count`, enforcing only the **format** ceiling `ARRAY_MAX`
//!    and sizing nothing on the strength of it;
//! 2. read the `fixlen_word` (element subtype + per-element length);
//! 3. a subtype that **contradicts** the declared element type makes the field a
//!    §7.3 skip — and a schema `count` bound MUST NOT be applied to it, because
//!    the field was never this array's value;
//! 4. only a field that survives step 3 is measured against the schema bound.
//!
//! The corelib is schema-agnostic, so steps 3–4 are the consumer's to make. What
//! the corelib owes the consumer is the *information and the ordering* that make
//! them possible: [`Visitor::array_begin`] fires **after** the `fixlen_word`, and
//! its [`ArrayKind`] names the element subtype on the wire (`Fp32` / `Fp64`)
//! rather than collapsing both into one "fixlen" category. Before F-0042 it
//! fired on the count word with a collapsed kind, so a consumer had to judge the
//! count before it could know the subtype — and a message truncated *between*
//! the two words was judged at all, where §4.8 requires INCOMPLETE.
//!
//! The vectors are the ones in the finding, at `arrays` (id 100) → `nested`
//! (id 10) → id 0, which a schema declares as `array<fp32, count 5>`:
//! `20` is the `fixlen_word` for fp32 (4 B), `41` for fp64 (8 B).

#![cfg(all(
    feature = "array",
    feature = "fixlen",
    feature = "fp64",
    feature = "sequence"
))]

mod common;

use common::{feed, push_varint, Event, Recorder};
use sofab::{ArrayKind, Error, IStream, OStream};

/// Wrap `body` in the finding's frame: sequence 100 (`arrays`) → sequence 10
/// (`nested`) → `body` → two sequence-ends.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xa6, 0x06, 0x56];
    v.extend_from_slice(body);
    v.extend_from_slice(&[0x07, 0x07]);
    v
}

/// `[ FIXLENARRAY id 0 ][ count ][ fixlen_word ]` + `payload` zero bytes, framed.
fn fixlen_array(count: u8, word: u8, payload: usize) -> Vec<u8> {
    let mut body = vec![0x05, count, word];
    body.resize(3 + payload, 0x00);
    framed(&body)
}

/// The frame's own events, as a prefix for the expected event list.
fn frame_begin() -> Vec<Event> {
    vec![Event::SequenceBegin(100), Event::SequenceBegin(10)]
}

// --- row 2: the primary vector ----------------------------------------------

#[test]
fn row2_mistyped_over_count_is_announced_as_fp64_after_the_word() {
    // `a6 06 56 05 08 41 00*64 07 07`: count 8 (over the schema's 5) but a
    // `fixlen_word` of 0x41 = fp64, contradicting the declared fp32. The
    // consumer must learn "fp64" *before* it can be tempted to apply the bound,
    // so the header event has to carry `Fp64` — not a collapsed fixlen kind.
    let bytes = fixlen_array(0x08, 0x41, 64);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));

    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp64, 8));
    expected.extend((0..8).map(|_| Event::Fp64(0, 0.0f64.to_bits())));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(events, expected);
}

#[test]
fn header_is_announced_only_once_the_fixlen_word_has_arrived() {
    // The ordering, byte by byte: the array is announced on the `fixlen_word`,
    // never on the count word. Feeding one byte at a time pins the exact byte.
    let bytes = fixlen_array(0x08, 0x41, 64);
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for (i, b) in bytes.iter().enumerate() {
        let _ = is.feed(&[*b], &mut rec);
        let announced = rec
            .events
            .iter()
            .any(|e| matches!(e, Event::ArrayBegin(..)));
        // byte 4 (0-based) is the count word, byte 5 the `fixlen_word`.
        assert_eq!(
            announced,
            i >= 5,
            "array_begin after byte {i} ({b:#04x}) — expected only from the fixlen_word on",
        );
    }
}

#[test]
fn header_hook_fires_once_per_array_never_per_element() {
    // Cost invariant: one header event for an 8-element array, not eight.
    let (_, events) = feed(&fixlen_array(0x08, 0x41, 64));
    let begins = events
        .iter()
        .filter(|e| matches!(e, Event::ArrayBegin(..)))
        .count();
    assert_eq!(begins, 1);
}

// --- row 4: truncation between the two words ---------------------------------

#[test]
fn row4_truncated_between_the_words_is_incomplete_and_announces_nothing() {
    // `a6 06 56 05 08`: EOF after the count word, before the `fixlen_word`. The
    // decoder cannot yet know whether this is a field it must bound, so §5.2's
    // precedence does not reach INVALID: the outcome is INCOMPLETE and nothing
    // has been announced that a consumer could reject on.
    let mut bytes = vec![0xa6, 0x06, 0x56, 0x05, 0x08];
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::Incomplete));
    assert_eq!(events, frame_begin());

    // One more byte — the `fixlen_word` — and the array is announced, still
    // INCOMPLETE (the payload is missing) but now decidable.
    bytes.push(0x41);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::Incomplete));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp64, 8));
    assert_eq!(events, expected);
}

// --- rows 3 and 5: the controls that prove the bound is reordered, not removed

#[test]
fn row3_matching_over_count_is_announced_as_fp32() {
    // `a6 06 56 05 08 20 00*32 07 07`: the `fixlen_word` 0x20 = fp32 *matches*
    // the declared element type, so the field survives §7.3 and the schema bound
    // (count 8 > declared 5) applies — which requires the consumer to be handed
    // `Fp32` together with the count 8. The corelib itself knows no schema and
    // accepts; the driver's INVALID verdict is built on exactly this event.
    let (outcome, events) = feed(&fixlen_array(0x08, 0x20, 32));
    assert_eq!(outcome, Ok(()));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp32, 8));
    expected.extend((0..8).map(|_| Event::Fp32(0, 0.0f32.to_bits())));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(events, expected);
}

#[test]
fn row5_matching_over_count_is_announced_before_any_payload_byte() {
    // `a6 06 56 05 08 20`: the word arrived and matches, so the over-count is
    // malformed regardless of what follows and the consumer must be able to say
    // so *now* — no element ever arrives to latch it on later. The header event
    // must therefore already be recorded, even though the corelib's own outcome
    // for these bytes is INCOMPLETE.
    let bytes = vec![0xa6, 0x06, 0x56, 0x05, 0x08, 0x20];
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::Incomplete));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp32, 8));
    assert_eq!(events, expected);
}

// --- row 6: the happy-path control -------------------------------------------

#[test]
fn row6_control_decodes_and_reencodes_byte_identically() {
    // `a6 06 56 05 03 20 00*12 07 07`: an in-bound, correctly typed fp32[3].
    let bytes = fixlen_array(0x03, 0x20, 12);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(()));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp32, 3));
    expected.extend((0..3).map(|_| Event::Fp32(0, 0.0f32.to_bits())));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(events, expected);

    // It is the one vector in the set whose re-encode equals its input.
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_sequence_begin_lazy(100).unwrap();
        os.write_sequence_begin_lazy(10).unwrap();
        os.write_array_fp32(0, &[0.0f32; 3]).unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
        os.bytes_used()
    };
    assert_eq!(&buf[..used], &bytes[..]);
}

// --- zero-count fixlen arrays (§4.8) -----------------------------------------

#[test]
fn zero_count_mistyped_array_is_announced_once_with_its_subtype() {
    // `a6 06 56 05 00 41 07 07`: a zero-count fixlen array still carries its
    // `fixlen_word`, so the header fires exactly once — after the word, with
    // `Fp64` and count 0 — and no payload is read. Moving the call site must not
    // drop the zero-count case.
    let (outcome, events) = feed(&fixlen_array(0x00, 0x41, 0));
    assert_eq!(outcome, Ok(()));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp64, 0));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(events, expected);
}

#[test]
fn empty_fp32_and_fp64_arrays_stay_distinguishable() {
    // The whole reason an empty fixlen array carries a `fixlen_word` at all.
    assert_eq!(
        feed(&[0x05, 0x00, 0x20]).1,
        [Event::ArrayBegin(0, ArrayKind::Fp32, 0)]
    );
    assert_eq!(
        feed(&[0x05, 0x00, 0x41]).1,
        [Event::ArrayBegin(0, ArrayKind::Fp64, 0)]
    );
}

#[test]
fn zero_count_array_truncated_before_its_word_is_incomplete() {
    // `05 00` is not yet a whole array header: the word is still owed.
    let (outcome, events) = feed(&[0x05, 0x00]);
    assert_eq!(outcome, Err(Error::Incomplete));
    assert!(events.is_empty());
}

// --- format violations in the `fixlen_word` (INVALID, never a §7.3 skip) -----

#[test]
fn illegal_element_subtype_is_invalid_and_announces_nothing() {
    // `a6 06 56 05 03 22 00*12 07 07`: subtype 2 (string) with elem_len 4. §4.8
    // permits only fp32/fp64 as fixlen-array elements, so this is a *format*
    // violation judged before the header fires — it must not be routed to the
    // §7.3 skip path even though the subtype also contradicts the declared fp32.
    for word in [0x22u8 /* string, len 4 */, 0x23 /* blob, len 4 */] {
        let (outcome, events) = feed(&fixlen_array(0x03, word, 12));
        assert_eq!(
            outcome,
            Err(Error::InvalidMsg),
            "fixlen_word {word:#04x} must be INVALID",
        );
        assert_eq!(
            events,
            frame_begin(),
            "nothing may be announced for {word:#04x}"
        );
    }
}

#[test]
fn element_width_mismatch_is_invalid_and_announces_nothing() {
    // fp32 must be 4 bytes and fp64 8; anything else is malformed, not a skip.
    for word in [0x40u8 /* fp32, len 8 */, 0x21 /* fp64, len 4 */] {
        let (outcome, events) = feed(&fixlen_array(0x03, word, 12));
        assert_eq!(
            outcome,
            Err(Error::InvalidMsg),
            "fixlen_word {word:#04x} must be INVALID",
        );
        assert_eq!(
            events,
            frame_begin(),
            "nothing may be announced for {word:#04x}"
        );
    }
}

// --- the format ceiling stays on the count word (§4.8 step 1) ----------------

#[test]
fn array_max_ceiling_still_fires_on_the_count_word() {
    // A fixlen array whose count is one past ARRAY_MAX is INVALID on the count
    // word alone — before the `fixlen_word` is read, and before anything is
    // announced. Moving the header hook must not drag the ceiling with it.
    let mut bytes = vec![0x05];
    push_varint(&mut bytes, 1u64 << 31);
    bytes.push(0x20); // a valid word, never reached
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::InvalidMsg));
    assert!(events.is_empty());
}

#[test]
fn count_at_array_max_waits_for_the_word_without_announcing() {
    // The boundary case: `ARRAY_MAX` itself passes the ceiling, so the decoder
    // simply waits for the `fixlen_word` — INCOMPLETE, nothing announced and
    // nothing sized on the strength of the count.
    let mut bytes = vec![0x05];
    push_varint(&mut bytes, (1u64 << 31) - 1);
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Err(Error::Incomplete));
    assert!(events.is_empty());
}

// --- integer arrays are untouched --------------------------------------------

#[test]
fn integer_array_header_still_fires_on_the_count_word() {
    // `a6 06 56 03 08 00*8 07 07`: an ARRAY_UNSIGNED header at the same slot.
    // There is no second word, so the header fires immediately after the count —
    // the reordering is confined to the fixlen path.
    let mut body = vec![0x03, 0x08];
    body.resize(2 + 8, 0x00);
    let bytes = framed(&body);

    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Unsigned, 8));
    expected.extend((0..8).map(|_| Event::Unsigned(0, 0)));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(feed(&bytes), (Ok(()), expected));

    // …and it is already announced when only the count has arrived.
    let (outcome, events) = feed(&bytes[..5]);
    assert_eq!(outcome, Err(Error::Incomplete));
    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Unsigned, 8));
    assert_eq!(events, expected);
}

#[test]
fn empty_integer_arrays_are_announced_on_the_count_word() {
    assert_eq!(
        feed(&[0x03, 0x00]).1,
        [Event::ArrayBegin(0, ArrayKind::Unsigned, 0)]
    );
    assert_eq!(
        feed(&[0x04, 0x00]).1,
        [Event::ArrayBegin(0, ArrayKind::Signed, 0)]
    );
}

// --- repeated occurrences at one id (MESSAGE_SPEC §7.4) -----------------------

#[test]
fn each_occurrence_at_one_id_carries_its_own_subtype() {
    // A correctly typed fp32 occurrence followed by a mis-typed fp64 one at the
    // same id: each is announced with its own kind, so a consumer can keep the
    // first and skip the second (a skipped occurrence is not an occurrence).
    let mut body = vec![0x05, 0x01, 0x20];
    body.resize(3 + 4, 0x00);
    body.extend_from_slice(&[0x05, 0x01, 0x41]);
    body.resize(7 + 3 + 8, 0x00);
    let (outcome, events) = feed(&framed(&body));
    assert_eq!(outcome, Ok(()));

    let mut expected = frame_begin();
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp32, 1));
    expected.push(Event::Fp32(0, 0.0f32.to_bits()));
    expected.push(Event::ArrayBegin(0, ArrayKind::Fp64, 1));
    expected.push(Event::Fp64(0, 0.0f64.to_bits()));
    expected.push(Event::SequenceEnd);
    expected.push(Event::SequenceEnd);
    assert_eq!(events, expected);
}
