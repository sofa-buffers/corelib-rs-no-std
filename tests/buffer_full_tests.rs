//! `BufferFull` on the sink-less encoder: where each writer stops when the
//! caller's buffer runs out, what it leaves behind, and what the documented
//! recovery reconstructs.
//!
//! A buffer installed **without** a flush sink is subject to no minimum
//! (CORELIB_PLAN §5.1): it either holds the message or reports `BufferFull`, and
//! the caller's recovery is to install a bigger buffer and retry the failed
//! write (README "Memory handling"). No writer here is atomic on failure — a cut
//! can fall inside a varint — but every writer is **prefix-exact**: what reached
//! the buffer is exactly the first `n` bytes of the one-shot encoding. That is
//! the property the recovery rests on, and it is what these tests drive, at
//! *every* cut position of every field kind rather than at one hand-picked one.
//!
//! The message-shaped half of the same story (a cut inside a held-back sequence
//! run, and the flush-sink path where a full buffer is uneventful) lives in
//! `ostream_tests.rs`; this file is about the per-writer boundary.
//!
//! Needs the full feature set, since it walks every writer the API has.

#![cfg(all(
    feature = "fixlen",
    feature = "fp64",
    feature = "array",
    feature = "sequence"
))]

use sofab::{Error, FixlenType, OStream, ID_MAX, LAZY_SEQ_DEPTH};

/// Room for the reference encode: larger than any field this suite writes.
const ROOMY: usize = 512;

/// Encode `f` into a buffer of exactly `cap` bytes; report its result and the
/// bytes that reached the buffer.
fn cut_at<F>(cap: usize, f: &F) -> (Result<(), Error>, Vec<u8>)
where
    F: Fn(&mut OStream) -> Result<(), Error>,
{
    let mut buf = vec![0u8; cap];
    let (res, used) = {
        let mut os = OStream::new(&mut buf);
        let res = f(&mut os);
        (res, os.bytes_used())
    };
    buf.truncate(used);
    (res, buf)
}

/// Drive `f` through **every** truncation of its own encoding: a buffer of any
/// size below the field's length must report `BufferFull` and leave exactly the
/// bytes that fitted, and a buffer of exactly that length must succeed.
/// Returns the one-shot bytes so the caller can pin the shape it just swept.
fn every_truncation_is_buffer_full<F>(f: F) -> Vec<u8>
where
    F: Fn(&mut OStream) -> Result<(), Error>,
{
    let (res, full) = cut_at(ROOMY, &f);
    assert_eq!(res, Ok(()), "the reference encode must fit");
    assert!(!full.is_empty(), "the reference encode must produce bytes");

    for cap in 0..full.len() {
        let (res, got) = cut_at(cap, &f);
        assert_eq!(
            res,
            Err(Error::BufferFull),
            "cut at {cap} of {}",
            full.len()
        );
        assert_eq!(got, full[..cap], "cut at {cap}: bytes left in the buffer");
    }

    let (res, got) = cut_at(full.len(), &f);
    assert_eq!(res, Ok(()), "an exactly-sized buffer must hold the field");
    assert_eq!(got, full);
    full
}

// --- scalars -----------------------------------------------------------------
//
// Id 300 makes the field header a two-byte varint, so the sweep also cuts
// *inside* a header rather than only between header and value.

#[test]
fn an_unsigned_field_reports_buffer_full_at_every_cut() {
    let full = every_truncation_is_buffer_full(|os| os.write_unsigned(300, 300));
    assert_eq!(full, [0xE0, 0x12, 0xAC, 0x02]);
}

#[test]
fn a_signed_field_reports_buffer_full_at_every_cut() {
    let full = every_truncation_is_buffer_full(|os| os.write_signed(300, -300));
    assert_eq!(full, [0xE1, 0x12, 0xD7, 0x04]);
}

#[test]
fn a_boolean_field_reports_buffer_full_at_every_cut() {
    every_truncation_is_buffer_full(|os| os.write_boolean(300, true));
}

// --- fixlen ------------------------------------------------------------------
//
// Three cut regions per field, all swept: the header, the `(len << 3) | subtype`
// word, and the payload itself — the one place a partial *payload* is left in
// the buffer.

#[test]
fn a_string_field_reports_buffer_full_at_every_cut() {
    every_truncation_is_buffer_full(|os| os.write_str(300, "a payload longer than one varint"));
}

#[test]
fn a_blob_field_reports_buffer_full_at_every_cut() {
    let data: Vec<u8> = (0..40u8).collect();
    every_truncation_is_buffer_full(|os| os.write_blob(300, &data));
    // The byte-taking primitive takes the same route.
    every_truncation_is_buffer_full(|os| os.write_fixlen(300, &data, FixlenType::Blob));
}

#[test]
fn float_fields_report_buffer_full_at_every_cut() {
    every_truncation_is_buffer_full(|os| os.write_fp32(300, 1.5));
    every_truncation_is_buffer_full(|os| os.write_fp64(300, -2.5));
}

// --- arrays ------------------------------------------------------------------
//
// Elements wide enough to be multi-byte varints, so a cut lands inside an
// element as well as between two.

#[test]
fn an_unsigned_array_reports_buffer_full_at_every_cut() {
    every_truncation_is_buffer_full(|os| os.write_array_unsigned(300, &[1u32, 200, 40_000]));
}

#[test]
fn a_signed_array_reports_buffer_full_at_every_cut() {
    every_truncation_is_buffer_full(|os| os.write_array_signed(300, &[-1i32, 200, -40_000]));
}

#[test]
fn a_float_array_reports_buffer_full_at_every_cut() {
    // Four regions here: header, count, `fixlen_word`, payload.
    every_truncation_is_buffer_full(|os| os.write_array_fp32(300, &[1.0f32, 2.0, 3.0]));
    every_truncation_is_buffer_full(|os| os.write_array_fp64(300, &[1.0f64, 2.0]));
}

#[test]
fn an_empty_float_array_still_reports_buffer_full_for_its_word() {
    // An empty fixlen array carries its `fixlen_word` regardless (§4.8), so the
    // buffer can run out on a field with no payload at all.
    let full = every_truncation_is_buffer_full(|os| os.write_array_fp32(1, &[]));
    assert_eq!(full, [0x0D, 0x00, 0x20]);
}

// --- sequences ---------------------------------------------------------------

#[test]
fn a_framed_sequence_reports_buffer_full_at_every_cut() {
    // A sequence with content: the opener holds its header back, the content
    // write commits it, and the closer emits the end marker — three writers
    // sharing one buffer, each of which can be the one that runs out.
    let full = every_truncation_is_buffer_full(|os| {
        os.write_sequence_begin_lazy(300)?;
        os.write_unsigned(1, 5)?;
        os.write_sequence_end()
    });
    assert_eq!(full, [0xE6, 0x12, 0x08, 0x05, 0x07]);
}

#[test]
fn an_out_of_range_id_is_refused_by_the_lazy_opener_before_anything_changes() {
    // The opener writes no bytes, so its id check is the only thing standing
    // between an out-of-range id and a header committed later, when the caller
    // is no longer at the call that supplied it.
    let mut buf = [0u8; 32];
    let mut os = OStream::new(&mut buf);

    assert_eq!(
        os.write_sequence_begin_lazy(ID_MAX + 1),
        Err(Error::Argument)
    );
    assert_eq!(os.bytes_used(), 0);
    assert_eq!(
        os.write_sequence_end(),
        Err(Error::Argument),
        "the depth budget was never spent, so there is nothing to close",
    );

    // The boundary itself is legal, and still costs nothing when it is dropped.
    assert_eq!(os.write_sequence_begin_lazy(ID_MAX), Ok(()));
    assert_eq!(os.write_sequence_end(), Ok(()));
    assert_eq!(os.bytes_used(), 0);
}

#[test]
fn framing_past_the_hold_back_window_reports_buffer_full_where_the_run_commits() {
    // Opening the `LAZY_SEQ_DEPTH + 1`-th sequence is the one opener that emits
    // bytes: it commits the held-back run and frames itself eagerly (README
    // "The bound"). Both halves of that can meet the end of the buffer — inside
    // the committed run, or at the eager header that follows it — and the
    // recovery is the documented one: install a bigger buffer and retry.
    const DEPTH: usize = LAZY_SEQ_DEPTH;
    let ids: Vec<sofab::Id> = (1..=DEPTH as sofab::Id + 1).collect();

    let one_shot = {
        let mut buf = [0u8; 64];
        let used = {
            let mut os = OStream::new(&mut buf);
            for &id in &ids {
                os.write_sequence_begin_lazy(id).unwrap();
            }
            for _ in &ids {
                os.write_sequence_end_keep().unwrap();
            }
            os.bytes_used()
        };
        buf[..used].to_vec()
    };
    // Ids 1..=9 are single-byte `SEQUENCE_START` headers, so every cut inside
    // the run lands on a header boundary; then one end marker per level.
    assert_eq!(
        one_shot,
        [
            0x0E, 0x16, 0x1E, 0x26, 0x2E, 0x36, 0x3E, 0x46, 0x4E, 0x07, 0x07, 0x07, 0x07, 0x07,
            0x07, 0x07, 0x07, 0x07
        ]
    );

    // `room == DEPTH` is the second half: the whole run fits and the eager
    // header of the ninth sequence does not.
    for room in 0..=DEPTH {
        let mut small = vec![0u8; room];
        let mut big = [0u8; 64];
        let (used_small, used_big) = {
            let mut os = OStream::new(&mut small);
            for &id in &ids[..DEPTH] {
                os.write_sequence_begin_lazy(id).unwrap();
            }
            assert_eq!(
                os.write_sequence_begin_lazy(ids[DEPTH]),
                Err(Error::BufferFull),
                "room = {room}",
            );
            let used_small = os.bytes_used();
            assert_eq!(
                used_small, room,
                "room = {room}: every byte that fitted is a committed header",
            );

            // The ids that never reached the buffer are still open sequences, so
            // retrying the failed opener picks the run up where it was cut.
            os.buffer_set(&mut big, 0).unwrap();
            os.write_sequence_begin_lazy(ids[DEPTH]).unwrap();
            for _ in &ids {
                os.write_sequence_end_keep().unwrap();
            }
            (used_small, os.bytes_used())
        };

        let mut got = small[..used_small].to_vec();
        got.extend_from_slice(&big[..used_big]);
        assert_eq!(got, one_shot, "room = {room}");
    }
}
