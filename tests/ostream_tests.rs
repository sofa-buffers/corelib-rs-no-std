//! Encoder tests. Every `expected` byte array is taken verbatim from the C
//! reference suite `test/c/test_ostream.c`.

// Float test vectors are deliberately the literals used by the C suite.
#![allow(clippy::approx_constant, clippy::excessive_precision)]

mod common;

use sofab::{Error, OStream, ID_MAX};

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

// --- ids --------------------------------------------------------------------

#[test]
fn id_min() {
    assert_eq!(encode(|os| os.write_unsigned(0, 0).unwrap()), [0x00, 0x00]);
}

#[test]
fn id_max() {
    assert_eq!(
        encode(|os| os.write_unsigned(ID_MAX, 0).unwrap()),
        [0xF8, 0xFF, 0xFF, 0xFF, 0x3F, 0x00]
    );
}

#[test]
fn id_overflow_is_argument_error() {
    let mut buf = [0u8; 16];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_unsigned(ID_MAX + 1, 0), Err(Error::Argument));
}

// --- unsigned varint (subset of the C boundary table) -----------------------

#[test]
fn write_unsigned_boundaries() {
    let cases: &[(u64, &[u8])] = &[
        (0, &[0x00, 0x00]),
        (127, &[0x00, 0x7F]),
        (128, &[0x00, 0x80, 0x01]),
        (0x3FFF, &[0x00, 0xFF, 0x7F]),
        (0x4000, &[0x00, 0x80, 0x80, 0x01]),
        (
            0x8000_0000_0000_0000,
            &[
                0x00, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            ],
        ),
        (
            u64::MAX,
            &[
                0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
            ],
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(
            encode(|os| os.write_unsigned(0, *value).unwrap()),
            *expected
        );
    }
}

// --- signed -----------------------------------------------------------------

#[test]
fn write_signed_min() {
    assert_eq!(
        encode(|os| os.write_signed(0, i64::MIN).unwrap()),
        [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]
    );
}

#[test]
fn write_signed_max() {
    assert_eq!(
        encode(|os| os.write_signed(0, i64::MAX).unwrap()),
        [0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]
    );
}

#[test]
fn write_boolean() {
    assert_eq!(
        encode(|os| os.write_boolean(0, true).unwrap()),
        [0x00, 0x01]
    );
}

// --- fixed length -----------------------------------------------------------

#[test]
fn write_fp32() {
    assert_eq!(
        encode(|os| os.write_fp32(0, 3.1415).unwrap()),
        [0x02, 0x20, 0x56, 0x0E, 0x49, 0x40]
    );
}

#[test]
fn write_fp64() {
    // The C test passes a float literal promoted to double: write_fp64(3.14159265f)
    assert_eq!(
        encode(|os| os.write_fp64(0, 3.14159265_f32 as f64).unwrap()),
        [0x02, 0x41, 0x00, 0x00, 0x00, 0x60, 0xFB, 0x21, 0x09, 0x40]
    );
}

#[test]
fn write_string() {
    assert_eq!(
        encode(|os| os.write_str(0, "Hello Couch!").unwrap()),
        [0x02, 0x62, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x20, 0x43, 0x6F, 0x75, 0x63, 0x68, 0x21]
    );
}

#[test]
fn write_string_empty() {
    assert_eq!(encode(|os| os.write_str(0, "").unwrap()), [0x02, 0x02]);
}

#[test]
fn write_blob() {
    assert_eq!(
        encode(|os| os.write_blob(0, &[0x01, 0x02, 0x03, 0x04, 0x05]).unwrap()),
        [0x02, 0x2B, 0x01, 0x02, 0x03, 0x04, 0x05]
    );
}

#[test]
fn write_blob_empty() {
    assert_eq!(encode(|os| os.write_blob(0, &[]).unwrap()), [0x02, 0x03]);
}

// --- varint arrays ----------------------------------------------------------

#[test]
fn write_array_of_u32() {
    let a: [u32; 5] = [1, 2, 3, 0x8000_0000, u32::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [
            0x03, 0x05, 0x01, 0x02, 0x03, 0x80, 0x80, 0x80, 0x80, 0x08, 0xFF, 0xFF, 0xFF, 0xFF,
            0x0F
        ]
    );
}

#[test]
fn write_array_of_i32() {
    let a: [i32; 5] = [-1, -2, -3, i32::MIN, i32::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [
            0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 0xFE, 0xFF, 0xFF, 0xFF,
            0x0F
        ]
    );
}

#[test]
fn write_array_of_i8() {
    let a: [i8; 5] = [-1, -2, -3, i8::MIN, i8::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0x01, 0xFE, 0x01]
    );
}

#[test]
fn write_array_of_u8() {
    let a: [u8; 5] = [1, 2, 3, 0, u8::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0x01]
    );
}

#[test]
fn write_array_of_i16() {
    let a: [i16; 5] = [-1, -2, -3, i16::MIN, i16::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0x03, 0xFE, 0xFF, 0x03]
    );
}

#[test]
fn write_array_of_u16() {
    let a: [u16; 5] = [1, 2, 3, 0, u16::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0xFF, 0x03]
    );
}

#[test]
fn write_array_of_i64() {
    let a: [i64; 5] = [-1, -2, -3, i64::MIN, i64::MAX];
    assert_eq!(
        encode(|os| os.write_array_signed(0, &a).unwrap()),
        [
            0x04, 0x05, 0x01, 0x03, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0x01, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01
        ]
    );
}

#[test]
fn write_array_of_u64() {
    let a: [u64; 5] = [1, 2, 3, 0, u64::MAX];
    assert_eq!(
        encode(|os| os.write_array_unsigned(0, &a).unwrap()),
        [
            0x03, 0x05, 0x01, 0x02, 0x03, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0x01
        ]
    );
}

// --- fixlen arrays ----------------------------------------------------------

#[test]
fn write_array_of_fp32() {
    let a: [f32; 5] = [1.0, 2.0, 3.0, -f32::MAX, f32::MAX];
    assert_eq!(
        encode(|os| os.write_array_fp32(0, &a).unwrap()),
        [
            0x05, 0x05, 0x20, 0x00, 0x00, 0x80, 0x3F, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x40,
            0x40, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF, 0x7F, 0x7F
        ]
    );
}

#[test]
fn write_array_of_fp64() {
    let a: [f64; 5] = [1.0, 2.0, 3.0, -f64::MAX, f64::MAX];
    assert_eq!(
        encode(|os| os.write_array_fp64(0, &a).unwrap()),
        [
            0x05, 0x05, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF0, 0x3F, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x40, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xEF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xEF,
            0x7F
        ]
    );
}

// --- sequences --------------------------------------------------------------

#[test]
fn write_nested_sequence() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_signed(2, -42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_signed(2, -42).unwrap();
    });
    assert_eq!(
        bytes,
        [0x00, 0x2A, 0x0E, 0x00, 0x2A, 0x11, 0x53, 0x07, 0x11, 0x53]
    );
}

#[test]
fn write_nested_sequence_with_array() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_begin_lazy(3).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_array_signed(3, &[-42_i32, -43, -44]).unwrap();
        os.write_sequence_end().unwrap();
        os.write_signed(2, -42).unwrap();
    });
    assert_eq!(
        bytes,
        [0x00, 0x2A, 0x1E, 0x00, 0x2A, 0x1C, 0x03, 0x53, 0x55, 0x57, 0x07, 0x11, 0x53]
    );
}

// --- lazy sequence framing (MESSAGE_SPEC §2) --------------------------------

/// An all-default sequence carries no information, so the field is omitted --
/// where the eager API would have written the two-byte empty frame `0E 07`.
#[test]
fn lazy_sequence_without_content_emits_nothing() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert!(bytes.is_empty(), "got {bytes:02x?}");
}

/// `end_keep` forces a contentless frame onto the wire — the array element and
/// explicit-empty cases of §2/§5.1.
#[test]
fn end_keep_frames_a_contentless_sequence() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end_keep().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x07]);
}

/// Forcing a frame forces its ancestors too: the outer sequence got content (the
/// inner frame), so it is framed as well.
#[test]
fn end_keep_commits_the_enclosing_run() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end_keep().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x16, 0x07, 0x07]);
}

/// With content it makes no difference — the headers are already out.
#[test]
fn end_keep_matches_end_once_content_exists() {
    let with_keep = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end_keep().unwrap();
    });
    let with_end = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(with_keep, [0x0E, 0x00, 0x2A, 0x07]);
    assert_eq!(with_keep, with_end);
}

/// One child field commits the whole held-back run, outermost header first, so a
/// non-default leaf deep inside brings every enclosing frame back in wire order.
#[test]
fn lazy_sequence_commits_the_whole_run_on_first_content() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x16, 0x00, 0x2A, 0x07, 0x07]);
}

/// Only the empty inner sequence drops; the outer one has content (the leaf) and
/// is framed. This is the interleaving the naive "drop the whole run" would get
/// wrong.
#[test]
fn lazy_sequence_drops_only_the_empty_inner_one() {
    let bytes = encode(|os| {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    });
    assert_eq!(bytes, [0x0E, 0x00, 0x2A, 0x07]);
}

/// A lazily framed sequence *after* content in the same scope, and the sibling
/// order, stay intact.
#[test]
fn lazy_sequence_after_content_is_independent() {
    let bytes = encode(|os| {
        os.write_unsigned(0, 1).unwrap();
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(2, 3).unwrap();
    });
    assert_eq!(bytes, [0x00, 0x01, 0x10, 0x03]);
}

/// A committed run split across a flush boundary produces exactly the one-shot
/// bytes: the same writes through a 3-byte flushing window and through a buffer
/// large enough for the whole message agree byte for byte.
///
/// Note what this deliberately does *not* claim to test. Held-back headers are
/// encoder state (`pending`/`npending`) and occupy no buffer space, so no flush
/// can land *before* a run starts committing. It can land in the middle of one,
/// though — `commit_pending` writes through `push_byte` like everything else —
/// and with a sink that is uneventful: the bytes go to the sink and the run
/// carries on. Without a sink the same cut is `BufferFull`, which is a different
/// test (`a_cut_run_keeps_the_ids_it_did_not_emit`). What is exercised here is
/// the boundary in the middle of the bytes a committed run produced (after
/// `0E 00 2A`, before the closing `07`).
#[test]
fn a_committed_run_survives_a_flush_boundary() {
    fn writes<F: sofab::Flush>(os: &mut OStream<F>) {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_begin_lazy(2).unwrap();
        os.write_sequence_end().unwrap();
        os.write_unsigned(0, 42).unwrap();
        os.write_sequence_end().unwrap();
    }

    let mut out: Vec<u8> = Vec::new();
    let mut buf = [0u8; 3]; // smaller than the message: at least one flush lands
    {
        let mut os = sofab::OStream::with_flush(&mut buf, 0, |d: &[u8]| out.extend_from_slice(d));
        writes(&mut os);
        os.flush();
    }

    // The same writes into a buffer large enough that no flush ever happens.
    let one_shot = encode(writes);

    assert_eq!(out, one_shot);
    assert_eq!(out, [0x0E, 0x00, 0x2A, 0x07]);
}

/// A run cut in half by `BufferFull` keeps the ids it did not get to emit.
///
/// Without a sink the buffer end is an error, and it can fall *inside*
/// `commit_pending` — between two `SEQUENCE_START` headers of one run. The
/// encoder must drop only the headers that actually reached the buffer: the rest
/// are still open sequences, and `write_sequence_end` will emit an end marker for
/// every one of them. Forgetting them produces a stream with fewer `begin`s than
/// `end`s — not a truncated message, a structurally broken one.
///
/// The recovery path is the documented one and the one
/// `buffer_set_switches_buffers` already exercises: install a bigger buffer and
/// retry the failed write. Concatenating the two buffers must reproduce the
/// one-shot encode byte for byte.
///
/// The cut is driven to *every* position inside the run (1..=3 bytes of room, so
/// the failure lands after the 1st, 2nd and 3rd header), because the pre-fix bug
/// lost a different number of ids at each one.
#[test]
fn a_cut_run_keeps_the_ids_it_did_not_emit() {
    // Ids 1..=3 -> single-byte headers 0x0E, 0x16, 0x1E, so every cut inside the
    // run lands on a header boundary — the case this recovery covers.
    let one_shot = encode(|os| {
        for id in 1..=3 {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..3 {
            os.write_sequence_end().unwrap();
        }
    });
    assert_eq!(one_shot, [0x0E, 0x16, 0x1E, 0x00, 0x2A, 0x07, 0x07, 0x07]);

    for room in 1..=3usize {
        let mut small = [0u8; 3];
        let mut big = [0u8; 64];
        let (used_small, used_big) = {
            let mut os = OStream::new(&mut small[..room]);
            for id in 1..=3 {
                os.write_sequence_begin_lazy(id).unwrap();
            }
            // The buffer runs out partway through the header run.
            assert_eq!(
                os.write_unsigned(0, 42),
                Err(Error::BufferFull),
                "room = {room}"
            );
            let used_small = os.bytes_used();
            // Documented recovery: hand the encoder a fresh buffer and retry.
            os.buffer_set(&mut big, 0);
            os.write_unsigned(0, 42).unwrap();
            for _ in 0..3 {
                os.write_sequence_end().unwrap();
            }
            (used_small, os.bytes_used())
        };

        let mut got = small[..used_small].to_vec();
        got.extend_from_slice(&big[..used_big]);
        assert_eq!(got, one_shot, "cut after {room} header byte(s)");
    }
}

/// The other caller of the run commit: `write_sequence_end_keep`, which forces an
/// empty frame out. It has to survive the same cut — this is the closer a
/// wrapper-array **element** uses (§5.1), where a lost `SEQUENCE_START` would not
/// merely unbalance the stream but change the decoded array length.
#[test]
fn a_cut_run_recovers_through_end_keep_too() {
    let one_shot = encode(|os| {
        for id in 1..=3 {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        for _ in 0..3 {
            os.write_sequence_end_keep().unwrap();
        }
    });
    assert_eq!(one_shot, [0x0E, 0x16, 0x1E, 0x07, 0x07, 0x07]);

    for room in 1..=2usize {
        let mut small = [0u8; 2];
        let mut big = [0u8; 64];
        let (used_small, used_big) = {
            let mut os = OStream::new(&mut small[..room]);
            for id in 1..=3 {
                os.write_sequence_begin_lazy(id).unwrap();
            }
            // `end_keep` commits the run first, so the buffer end falls inside it.
            assert_eq!(
                os.write_sequence_end_keep(),
                Err(Error::BufferFull),
                "room = {room}"
            );
            let used_small = os.bytes_used();
            os.buffer_set(&mut big, 0);
            // Retrying the failed closer picks the run up where it was cut; the
            // depth budget was never spent, so all three still close.
            for _ in 0..3 {
                os.write_sequence_end_keep().unwrap();
            }
            (used_small, os.bytes_used())
        };

        let mut got = small[..used_small].to_vec();
        got.extend_from_slice(&big[..used_big]);
        assert_eq!(got, one_shot, "cut after {room} header byte(s)");
    }
}

// --- the hold-back window (LAZY_SEQ_DEPTH, CORELIB_PLAN §6) ------------------
//
// This is the heap-free profile of CORELIB_PLAN §6 "How deep the hold-back
// reaches": a port that can allocate must hold back to the full MAX_DEPTH and is
// canonical at every depth; this one bounds the pending run at `LAZY_SEQ_DEPTH`
// and frames eagerly beyond it, which is well-formed and decodes to the same
// value but is *not* canonical. The bound is therefore observable in the bytes,
// so it is pinned by tests — changing `LAZY_SEQ_DEPTH` must fail here (and be
// re-documented in the README) rather than silently changing what this encoder
// puts on the wire.
//
// Every test below nests a **distinct id per level** (level `n` is id `n`), so
// the expected bytes pin the *order* the headers come out in and not just how
// many there are. Nesting the same id at every level makes each held-back header
// the identical byte `0x0E`, which leaves the commit order unobservable: an
// encoder that framed a sequence before its own ancestors would produce the same
// byte string, and the decoder cannot tell either — the result is still a
// well-nested (but differently shaped) tree.

/// The `SEQUENCE_START` header bytes for `id`, so a test can expect a nest of
/// distinct ids. Ids past 15 need a two-byte varint, which is exactly why this is
/// built rather than written out as literals.
fn seq_start(id: sofab::Id) -> Vec<u8> {
    let mut out = Vec::new();
    common::push_varint(&mut out, ((id as u64) << 3) | 6); // 6 = T_SEQUENCE_START
    out
}

/// Concatenated `SEQUENCE_START` headers for levels `1..=n`, outermost first.
fn seq_starts(n: usize) -> Vec<u8> {
    (1..=n as sofab::Id).flat_map(seq_start).collect()
}

/// At the window's edge the canonical result still holds: `LAZY_SEQ_DEPTH`
/// nested sequences, all contentless, vanish completely.
#[test]
fn contentless_nesting_within_the_window_emits_nothing() {
    let bytes = encode(|os| {
        for id in 1..=sofab::LAZY_SEQ_DEPTH as sofab::Id {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        for _ in 0..sofab::LAZY_SEQ_DEPTH {
            os.write_sequence_end().unwrap();
        }
    });
    assert!(bytes.is_empty(), "got {bytes:02x?}");
}

/// One level past the window, the documented fallback kicks in: opening the
/// `LAZY_SEQ_DEPTH + 1`-th sequence commits the whole held-back run and frames
/// itself eagerly, so all of them keep the empty frame §2 would have omitted.
/// Non-canonical, but well-formed — and it decodes back to the same (all-default)
/// value, which is why the profile is allowed to do it.
///
/// The ids also pin the *order*: the ninth sequence must commit its eight
/// ancestors first and frame itself last, so the headers read `1 2 … 9` and not
/// `9 1 2 … 8`. Both orders are well-nested and decode without complaint, so the
/// distinct ids are the only thing that can tell them apart.
#[test]
fn contentless_nesting_one_past_the_window_keeps_every_frame() {
    const DEPTH: usize = sofab::LAZY_SEQ_DEPTH + 1;
    let bytes = encode(|os| {
        for id in 1..=DEPTH as sofab::Id {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
    });
    // Headers for ids 1..=DEPTH, outermost first; end marker = 0x07.
    let mut expected = seq_starts(DEPTH);
    expected.extend(std::iter::repeat(0x07).take(DEPTH));
    assert_eq!(bytes, expected);
}

/// Deep nesting, far past the window: 40 contentless levels. A port that holds
/// back to MAX_DEPTH emits zero bytes here; this one emits the frames of every
/// level that was pushed out of the window, and only the innermost partial run
/// still vanishes.
///
/// The count is exact, not approximate. Each fallback cycle consumes
/// `LAZY_SEQ_DEPTH + 1` levels — eight fill the window, the ninth commits them
/// and frames itself — so with a window of 8 the commits happen at levels 9, 18,
/// 27 and 36: levels 1..=36 are framed, while levels 37..=40 are still held back
/// when they close and disappear.
///
/// With one id per level the expected bytes say *which* levels those are — ids
/// 1..=36 in ascending order — rather than only how many headers appeared.
#[test]
fn contentless_nesting_far_past_the_window_frames_all_but_the_last_run() {
    const DEPTH: usize = 40;
    let cycle = sofab::LAZY_SEQ_DEPTH + 1;
    let framed = DEPTH / cycle * cycle; // 36 at the default window of 8
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        for id in 1..=DEPTH as sofab::Id {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let bytes = buf[..used].to_vec();

    let mut expected = seq_starts(framed);
    expected.extend(std::iter::repeat(0x07).take(framed));
    assert_eq!(bytes, expected, "framed levels: {framed}");
}

/// Content deep inside a nest far past the window still comes out in wire order:
/// the eager fallback commits ancestors in the same outermost-first order as
/// `commit_pending`, so the frames are properly nested around the leaf.
///
/// This is the test that needs distinct ids most. Nesting id 1 forty times makes
/// every header the byte `0x0E`, and an encoder that framed each window-full
/// sequence *before* its own ancestors would emit the identical byte string —
/// and a stream that still decodes as a clean 40-deep nest, just with the wrong
/// sequence at the wrong depth. With one id per level the ancestors-first commit
/// order is what the bytes assert.
#[test]
fn content_past_the_window_keeps_wire_order() {
    const DEPTH: usize = 40;
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        for id in 1..=DEPTH as sofab::Id {
            os.write_sequence_begin_lazy(id).unwrap();
        }
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let mut expected = seq_starts(DEPTH);
    expected.extend([0x00, 0x2A]);
    expected.extend(std::iter::repeat(0x07).take(DEPTH));
    assert_eq!(buf[..used], expected[..]);
}

// --- error / overflow behavior ---------------------------------------------

#[test]
fn buffer_full_without_sink() {
    let mut buf = [0u8; 2];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_unsigned(0, u64::MAX), Err(Error::BufferFull));
}

// --- zero-count arrays (§4.7/§4.8) ------------------------------------------

#[test]
fn empty_unsigned_array_encodes_header_and_zero_count() {
    // §4.7: a zero-count array is exactly `[ header ][ count = 0 ]`.
    let empty: [u32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_unsigned(7, &empty).unwrap()),
        [0x3B, 0x00] // id 7, type 0b011 (unsigned array) -> 0x3B; count 0
    );
}

#[test]
fn empty_signed_array_encodes_header_and_zero_count() {
    let empty: [i32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_signed(7, &empty).unwrap()),
        [0x3C, 0x00] // id 7, type 0b100 (signed array) -> 0x3C; count 0
    );
}

#[test]
fn empty_fp32_array_carries_fixlen_word() {
    // §4.8: a zero-count fixlen array still carries its `fixlen_word` (but no
    // payload) so an empty fp32 array stays distinct from an empty fp64 one.
    let empty: [f32; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_fp32(7, &empty).unwrap()),
        [0x3D, 0x00, 0x20] // id 7, fixlen array -> 0x3D; count 0; fixlen_word (4<<3)|fp32
    );
}

#[test]
fn empty_fp64_array_carries_fixlen_word() {
    let empty: [f64; 0] = [];
    assert_eq!(
        encode(|os| os.write_array_fp64(7, &empty).unwrap()),
        [0x3D, 0x00, 0x41] // id 7, fixlen array -> 0x3D; count 0; fixlen_word (8<<3)|fp64
    );
}

// --- nesting depth (§4.9/§6.2, MAX_DEPTH = 255) -----------------------------

#[test]
fn sequence_depth_over_max_is_argument_error() {
    let mut buf = [0u8; 512];
    let mut os = OStream::new(&mut buf);
    // Opening MAX_DEPTH (255) nested sequences is fine.
    for _ in 0..255 {
        os.write_sequence_begin_lazy(0).unwrap();
    }
    // The 256th must be rejected without writing anything.
    assert_eq!(os.write_sequence_begin_lazy(0), Err(Error::Argument));
}

/// Depth bookkeeping, the invariant the `MAX_DEPTH` guard rests on: every closer
/// must give the budget back. Both closers and both open paths (held back, and
/// eagerly framed past the window) are exercised — a `depth` that is not
/// decremented on the drop path would make a long-running encoder refuse
/// sequences after 255 of them, and one decremented twice would let a message
/// nest past `MAX_DEPTH`.
#[test]
fn closing_a_sequence_returns_the_depth_budget() {
    const MAX: u32 = sofab::MAX_DEPTH;
    let mut buf = [0u8; 4096];
    let mut os = OStream::new(&mut buf);

    for closer in 0..2 {
        // Fill the budget exactly, then prove it is full.
        for _ in 0..MAX {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));
        // Give it all back — with `end` on the first pass, `end_keep` on the
        // second, so both closers are shown to decrement exactly once.
        for _ in 0..MAX {
            if closer == 0 {
                os.write_sequence_end().unwrap();
            } else {
                os.write_sequence_end_keep().unwrap();
            }
        }
        // Budget restored: the whole nest opens again, and still stops at MAX.
        for _ in 0..MAX {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));
        for _ in 0..MAX {
            os.write_sequence_end().unwrap();
        }
    }
}

/// The drop path on its own: repeatedly opening and closing a contentless
/// sequence must neither emit bytes nor consume depth, however often it happens.
#[test]
fn dropped_sequences_leak_neither_bytes_nor_depth() {
    let mut buf = [0u8; 512];
    let mut os = OStream::new(&mut buf);
    for _ in 0..(sofab::MAX_DEPTH * 4) {
        os.write_sequence_begin_lazy(1).unwrap();
        os.write_sequence_end().unwrap();
    }
    assert_eq!(os.bytes_used(), 0);
    // Depth is back at zero, so a full-depth nest still opens.
    for _ in 0..sofab::MAX_DEPTH {
        os.write_sequence_begin_lazy(1).unwrap();
    }
    assert_eq!(os.write_sequence_begin_lazy(1), Err(Error::Argument));
}

// --- streaming flush sink ---------------------------------------------------

#[test]
fn flush_sink_streams_large_message() {
    // A 4-byte buffer cannot hold the whole message; the flush sink must
    // receive the overflow so the full byte stream is reconstructed.
    let mut collected: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4];
    {
        let mut os = OStream::with_flush(&mut buf, 0, |chunk: &[u8]| {
            collected.extend_from_slice(chunk);
        });
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
        os.flush();
    }

    // Reference: the same writes into one large buffer.
    let reference = encode(|os| {
        for i in 0..10u32 {
            os.write_unsigned(i, i as u64).unwrap();
        }
    });
    assert_eq!(collected, reference);
}
