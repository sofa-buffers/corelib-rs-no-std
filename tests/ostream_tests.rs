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
/// Note what this deliberately does *not* claim to test. A flush cannot land
/// while a run is still held back — that is unreachable **by construction**, not
/// merely untested: held-back headers are encoder state (`pending`/`npending`),
/// they occupy no buffer space, and the buffer only ever fills inside
/// `push_byte`, which is reached from a write that has already committed the run
/// in `write_id_type`. So the interesting boundary is the one exercised here:
/// the run is committed first, and the flush then falls in the middle of the
/// bytes it produced (here after `0E 00 2A`, before the closing `07`).
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

/// At the window's edge the canonical result still holds: `LAZY_SEQ_DEPTH`
/// nested sequences, all contentless, vanish completely.
#[test]
fn contentless_nesting_within_the_window_emits_nothing() {
    let bytes = encode(|os| {
        for _ in 0..sofab::LAZY_SEQ_DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
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
#[test]
fn contentless_nesting_one_past_the_window_keeps_every_frame() {
    const DEPTH: usize = sofab::LAZY_SEQ_DEPTH + 1;
    let bytes = encode(|os| {
        for _ in 0..DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
    });
    // id 1 -> header (1 << 3) | SEQUENCE_START = 0x0E; end marker = 0x07.
    let mut expected = vec![0x0E; DEPTH];
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
#[test]
fn contentless_nesting_far_past_the_window_frames_all_but_the_last_run() {
    const DEPTH: usize = 40;
    let cycle = sofab::LAZY_SEQ_DEPTH + 1;
    let framed = DEPTH / cycle * cycle; // 36 at the default window of 8
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        for _ in 0..DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let bytes = buf[..used].to_vec();

    let mut expected = vec![0x0E; framed];
    expected.extend(std::iter::repeat(0x07).take(framed));
    assert_eq!(bytes, expected, "framed levels: {framed}");
}

/// Content deep inside a nest far past the window still comes out in wire order:
/// the eager fallback commits ancestors in the same outermost-first order as
/// `commit_pending`, so the frames are properly nested around the leaf.
#[test]
fn content_past_the_window_keeps_wire_order() {
    const DEPTH: usize = 40;
    let mut buf = [0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        for _ in 0..DEPTH {
            os.write_sequence_begin_lazy(1).unwrap();
        }
        os.write_unsigned(0, 42).unwrap();
        for _ in 0..DEPTH {
            os.write_sequence_end().unwrap();
        }
        os.bytes_used()
    };
    let mut expected = vec![0x0E; DEPTH];
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
