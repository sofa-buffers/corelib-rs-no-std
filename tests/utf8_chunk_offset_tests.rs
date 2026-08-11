//! Strict UTF-8 at a **non-zero chunk offset** — the hole the shared
//! `invalid_utf8` vectors leave open.
//!
//! The shared vectors (corelib-c-cpp#97, replayed by `utf8_tests.rs`) put the
//! `string` field first in the message and feed it whole, so every
//! [`Visitor::string`] callback they produce arrives at `offset == 0` with
//! `chunk.len() == total`. A consumer whose validation is **offset-sensitive** —
//! one handed a length where an exclusive end index was required, the defect
//! that motivated this suite — therefore replays the entire shared corpus while
//! accepting *every* invalid input. A green conformance run says nothing about
//! the offset-carrying path.
//!
//! This suite closes that gap for this port. Same payloads, delivered so that
//! the invalid sequence starts at a chunk offset at or beyond the bytes fed so
//! far: the `string` field placed far into the buffer behind a long `blob`, and
//! its payload split so the offending bytes arrive alone in a later chunk.
//!
//! It also pins the three normative outcome rules of CORELIB_PLAN §6.4
//! ("cross-chunk semantics"), which decide *when* a verdict may be reported:
//!
//! * a multi-byte sequence split at an **end-of-chunk** is a well-formed prefix
//!   → `INCOMPLETE`, never `INVALID` and never a dropped string;
//! * a sequence truncated at **end-of-payload** (the declared length is reached
//!   mid-sequence) → `INVALID`;
//! * a byte that can neither begin nor continue a sequence is reported **at
//!   payload completion**, not before — the one place §5.2's
//!   INVALID-dominates-INCOMPLETE precedence does not pull the verdict forward.
//!
//! Division of responsibility is unchanged (§6.4, and see `utf8_tests.rs`): the
//! corelib hands over raw bytes and validates nothing, so each test asserts two
//! verdicts — the corelib's own structural outcome and the consumer's
//! materialization verdict.

#![cfg(feature = "fixlen")]

mod common;

use common::{hex_to_bytes, push_varint};
use serde_json::Value;
use sofab::{Error, IStream, Id, Unsigned, Visitor};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

/// The shared `invalid_utf8` negative vectors (tracks corelib-c-cpp#97).
fn invalid_utf8_vectors() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["invalid_utf8"]
        .as_array()
        .expect("invalid_utf8 array")
        .clone()
}

// --- crafting the wire bytes -------------------------------------------------
//
// Built by hand on purpose: this port's encode API cannot produce an
// invalid-UTF-8 `string` at all (`write_str` takes `&str`, and `write_fixlen`
// refuses the `string` subtype — `utf8_tests.rs` pins exactly that), so such a
// field can only arrive from a wire written elsewhere. That is what this suite
// feeds.

const T_FIXLEN: u64 = 0x2;
const T_VARINT_UNSIGNED: u64 = 0x0;
const SUB_STR: u64 = 0x2;
const SUB_BLOB: u64 = 0x3;

/// One fixlen field: `[ header ][ (len << 3) | subtype ][ payload ]`.
fn fixlen_field(id: Id, subtype: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, ((id as u64) << 3) | T_FIXLEN);
    push_varint(&mut out, ((payload.len() as u64) << 3) | subtype);
    out.extend_from_slice(payload);
    out
}

/// A `string` field carrying `payload` verbatim.
fn string_field(id: Id, payload: &[u8]) -> Vec<u8> {
    fixlen_field(id, SUB_STR, payload)
}

/// A `blob` field of `len` filler bytes — the ballast that pushes the field
/// under test far past its own length into the buffer.
fn blob_field(id: Id, len: usize) -> Vec<u8> {
    fixlen_field(id, SUB_BLOB, &vec![0x5A; len])
}

/// An unsigned field, to prove decoding carries on past a skipped string.
fn unsigned_field(id: Id, value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, ((id as u64) << 3) | T_VARINT_UNSIGNED);
    push_varint(&mut out, value);
    out
}

/// How much ballast precedes the field under test. Comfortably larger than any
/// payload here, so a consumer that confused a buffer position with a field
/// offset is out of range rather than subtly wrong.
const BALLAST: usize = 200;

/// Largest payload this suite materializes.
const CAP: usize = 512;

// --- the consumer ------------------------------------------------------------

/// The generated-code shape for a `string` field on this port: a fixed-capacity
/// destination in the message struct, every chunk copied in **at the offset the
/// corelib reports**, and `core::str::from_utf8` run once the payload is
/// complete (§6.4: validity is a property of the whole payload, and a chunk
/// boundary must not change it).
struct Strict {
    dst: [u8; CAP],
    /// `(total, offset, chunk.len())` of every `string` callback, in order.
    calls: Vec<(usize, usize, usize)>,
    /// The materialization verdict, set exactly once — at payload completion.
    verdict: Option<Result<(), Error>>,
}

impl Strict {
    fn new() -> Self {
        Strict {
            dst: [0; CAP],
            calls: Vec::new(),
            verdict: None,
        }
    }
}

impl Visitor for Strict {
    fn string(&mut self, _id: Id, total: usize, offset: usize, chunk: &[u8]) {
        let end = offset + chunk.len();
        // The chunking contract the destination copy rests on: offsets are
        // within the field, monotonic, and never run past the declared length.
        assert!(
            end <= total && total <= CAP,
            "chunk out of range: total={total} offset={offset} len={}",
            chunk.len(),
        );
        self.calls.push((total, offset, chunk.len()));
        self.dst[offset..end].copy_from_slice(chunk);
        if end == total {
            self.verdict = Some(
                core::str::from_utf8(&self.dst[..total])
                    .map(|_| ())
                    .map_err(|_| Error::InvalidMsg),
            );
        }
    }
}

/// Feed `bytes` through one decoder in chunks of the given byte counts,
/// returning the consumer and the outcome of every `feed`.
///
/// A corelib-level `INVALID` fails the test outright: every frame built here is
/// structurally well-formed, so the only verdict under test is the consumer's.
fn feed_pieces(bytes: &[u8], pieces: &[usize]) -> (Strict, Vec<Result<(), Error>>) {
    assert_eq!(
        pieces.iter().sum::<usize>(),
        bytes.len(),
        "the pieces must cover the message"
    );
    let mut sink = Strict::new();
    let mut is = IStream::new();
    let mut at = 0;
    let mut outcomes = Vec::new();
    for &n in pieces {
        let outcome = is.feed(&bytes[at..at + n], &mut sink);
        assert_ne!(
            outcome,
            Err(Error::InvalidMsg),
            "the corelib rejected a structurally well-formed frame",
        );
        outcomes.push(outcome);
        at += n;
    }
    (sink, outcomes)
}

/// One `feed` of the whole message.
fn feed_whole(bytes: &[u8]) -> (Strict, Result<(), Error>) {
    let (sink, outcomes) = feed_pieces(bytes, &[bytes.len()]);
    let last = *outcomes.last().unwrap();
    (sink, last)
}

// --- the gap -----------------------------------------------------------------

#[test]
fn a_chunk_offset_is_field_relative_never_buffer_relative() {
    // The root cause of the gap: a consumer keyed on where the bytes sit in the
    // buffer instead of where they sit in the field. Moving the field 200+ bytes
    // into the message must not move a single reported offset.
    let payload = "übergroß".as_bytes();
    let early = string_field(1, payload);
    let mut late = blob_field(2, BALLAST);
    late.extend_from_slice(&early);
    assert!(late.len() > BALLAST + payload.len());

    let (first, ra) = feed_whole(&early);
    let (behind_ballast, rb) = feed_whole(&late);

    assert_eq!(ra, Ok(()));
    assert_eq!(rb, Ok(()));
    assert_eq!(first.calls, [(payload.len(), 0, payload.len())]);
    assert_eq!(
        behind_ballast.calls, first.calls,
        "the {BALLAST}-byte blob in front must not move the string's own offsets",
    );
    assert_eq!(first.verdict, Some(Ok(())));
    assert_eq!(behind_ballast.verdict, Some(Ok(())));
}

#[test]
fn an_invalid_sequence_past_the_bytes_fed_so_far_is_rejected() {
    // 60 filler bytes, then a three-byte sequence with its last byte missing:
    // the declared length is reached mid-sequence, which §6.4 maps to INVALID —
    // no further byte belongs to this string.
    let mut payload = vec![b'x'; 60];
    payload.extend_from_slice(&[0xE2, 0x82]);
    let mut msg = blob_field(2, BALLAST);
    msg.extend_from_slice(&string_field(1, &payload));

    // One shot: the corelib is satisfied (it validates nothing) and the consumer
    // is not.
    let (one_shot, outcome) = feed_whole(&msg);
    assert_eq!(outcome, Ok(()), "structurally the frame is COMPLETE");
    assert_eq!(one_shot.verdict, Some(Err(Error::InvalidMsg)));

    // Now with the offending bytes alone in the final chunk. Their field offset
    // (60) is past that chunk *and* past the whole final feed — the shape no
    // shared vector produces, and the one an offset-sensitive validator gets
    // wrong.
    let (split, outcomes) = feed_pieces(&msg, &[msg.len() - 2, 2]);
    assert_eq!(outcomes.last(), Some(&Ok(())));
    assert_eq!(
        split.calls.last(),
        Some(&(62, 60, 2)),
        "the last chunk carries two bytes at field offset 60",
    );
    assert_eq!(
        split.verdict,
        Some(Err(Error::InvalidMsg)),
        "a chunk boundary must not change the outcome (§6.4)",
    );
}

#[test]
fn the_verdict_survives_every_split_point_and_a_byte_at_a_time_feed() {
    let mut payload = vec![b'x'; 60];
    payload.extend_from_slice(&[0xE2, 0x82]);
    let mut msg = blob_field(2, BALLAST);
    msg.extend_from_slice(&string_field(1, &payload));

    for cut in 0..=msg.len() {
        let (sink, _) = feed_pieces(&msg, &[cut, msg.len() - cut]);
        assert_eq!(
            sink.verdict,
            Some(Err(Error::InvalidMsg)),
            "split at byte {cut}",
        );
    }

    // The pathological split: one byte per feed, so every payload byte arrives
    // in its own callback at its own offset.
    let (sink, outcomes) = feed_pieces(&msg, &vec![1; msg.len()]);
    assert_eq!(outcomes.last(), Some(&Ok(())));
    assert_eq!(sink.verdict, Some(Err(Error::InvalidMsg)));
    let expected: Vec<(usize, usize, usize)> =
        (0..payload.len()).map(|i| (payload.len(), i, 1)).collect();
    assert_eq!(
        sink.calls, expected,
        "each byte must be announced at its own field offset",
    );
}

#[test]
fn every_shared_invalid_payload_stays_invalid_late_in_the_buffer() {
    // The shared corpus, replayed in the shape it never covers: behind ballast,
    // and split so the payload's tail arrives at a non-zero offset.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let id = v["id"].as_u64().unwrap() as Id;
        let raw = hex_to_bytes(v["string_hex"].as_str().unwrap());
        assert!(!raw.is_empty(), "[{name}] needs a payload to split");

        let mut msg = blob_field(31, BALLAST);
        msg.extend_from_slice(&string_field(id, &raw));

        let (whole, outcome) = feed_whole(&msg);
        assert_eq!(outcome, Ok(()), "[{name}] the frame itself is well-formed");
        assert_eq!(
            whole.verdict,
            Some(Err(Error::InvalidMsg)),
            "[{name}] one shot, late in the buffer",
        );

        // The tail alone in the final chunk: the last payload byte is announced
        // at offset `raw.len() - 1`, with only one byte fed alongside it.
        let (tail, _) = feed_pieces(&msg, &[msg.len() - 1, 1]);
        assert_eq!(
            tail.calls.last(),
            Some(&(raw.len(), raw.len() - 1, 1)),
            "[{name}] the tail must arrive at its own offset",
        );
        assert_eq!(
            tail.verdict,
            Some(Err(Error::InvalidMsg)),
            "[{name}] tail split",
        );

        let (bytewise, _) = feed_pieces(&msg, &vec![1; msg.len()]);
        assert_eq!(
            bytewise.verdict,
            Some(Err(Error::InvalidMsg)),
            "[{name}] byte at a time",
        );
    }
}

// --- §6.4 cross-chunk semantics ----------------------------------------------

#[test]
fn a_multibyte_sequence_split_at_a_chunk_boundary_stays_valid() {
    // The false-positive direction, and the §5.2 anti-folding rule: a chunk that
    // ends in the middle of a well-formed sequence is INCOMPLETE, not INVALID
    // and not a dropped string. A consumer that validated per chunk instead of
    // at payload completion would reject this message.
    let text = "héllo wörld";
    let msg = string_field(1, text.as_bytes());
    // 0xC3 is the lead byte of `é`; cut between it and its continuation byte.
    let lead = msg.iter().position(|&b| b == 0xC3).expect("lead byte");

    let (sink, outcomes) = feed_pieces(&msg, &[lead + 1, msg.len() - lead - 1]);
    assert_eq!(
        outcomes[0],
        Err(Error::Incomplete),
        "a split multi-byte sequence is a well-formed prefix",
    );
    assert_eq!(outcomes[1], Ok(()));
    assert_eq!(sink.verdict, Some(Ok(())), "the payload is valid UTF-8");
    assert_eq!(&sink.dst[..text.len()], text.as_bytes());
}

#[test]
fn a_malformed_byte_is_not_reported_before_the_payload_completes() {
    // §6.4: a byte that can neither begin nor continue a sequence is malformed
    // regardless of what follows, but the verdict is still reported **at payload
    // completion**. Until the declared length is reached the outcome is
    // INCOMPLETE and the consumer has decided nothing.
    let mut payload = vec![0xFF];
    payload.extend_from_slice(&[b'y'; 40]);
    let msg = string_field(1, &payload);
    let head = msg.len() - 40; // header, length word, and the 0xFF

    let mut sink = Strict::new();
    let mut is = IStream::new();
    assert_eq!(is.feed(&msg[..head], &mut sink), Err(Error::Incomplete));
    assert_eq!(
        sink.verdict, None,
        "no verdict may be reported mid-payload (§6.4)",
    );
    assert_eq!(is.feed(&msg[head..], &mut sink), Ok(()));
    assert_eq!(sink.verdict, Some(Err(Error::InvalidMsg)));
}

#[test]
fn a_string_the_consumer_never_reads_is_never_validated() {
    // §6.4 "skipped fields are never validated": validation runs where a string
    // is *materialized*. A consumer that does not handle `string` walks the
    // payload, the message stays COMPLETE, and the field behind it decodes.
    #[derive(Default)]
    struct OnlyUnsigned {
        seen: Vec<(Id, Unsigned)>,
    }
    impl Visitor for OnlyUnsigned {
        fn unsigned(&mut self, id: Id, value: Unsigned) {
            self.seen.push((id, value));
        }
    }

    let mut msg = string_field(1, &[0xFF, 0xFE, 0xC0, 0x80]);
    msg.extend_from_slice(&unsigned_field(2, 7));

    let mut sink = OnlyUnsigned::default();
    assert_eq!(IStream::new().feed(&msg, &mut sink), Ok(()));
    assert_eq!(sink.seen, [(2, 7)]);
}
