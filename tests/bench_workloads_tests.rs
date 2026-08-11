//! The BENCH_SPEC datasets, asserted in the ordinary test job (CORELIB_PLAN §10).
//!
//! `benches/bench.rs` measures these workloads; nothing about a measurement says
//! it measured the right thing. Every way these rows can degenerate makes them
//! print a *better* number — a chunked decode that goes `INVALID` on the first
//! chunk walks 244 chunks of nothing very quickly, a streaming encode whose sink
//! is never called is an encode into a 4 KB buffer, and an encoder that frames the
//! all-default field of the `composite` message still prints a `composite` row.
//! So the shape of each workload is checked here, where CI runs it, rather than
//! left to whoever reads the table.
//!
//! The parity sizes are the cross-port half of the same idea: BENCH_SPEC states
//! `blob 1MB` at 1,000,005 bytes and takes `composite`'s size from the reference
//! implementation, and a port whose bytes differ is not running the same workload
//! whatever its MB/s says.

#[path = "../benches/support/workloads.rs"]
mod workloads;

use sofab::{IStream, OStream};
use workloads::*;

/// `u64 array (1000)`: the dataset generator BENCH_SPEC spells out, and its
/// encoding as the decode rows consume it.
#[test]
fn u64_array_dataset_is_the_specified_one() {
    let src = make_src();
    assert_eq!(src.len(), N);
    assert_eq!(src[0], 0);
    assert_eq!(src[1], K);
    assert_eq!(src[999], 999u64.wrapping_mul(K));

    let wire = u64_array_wire(&src);
    let mut seen = Seen::default();
    IStream::new()
        .feed(&wire, &mut seen)
        .expect("u64 array decodes COMPLETE");
    assert_eq!(seen.scalars.len(), N, "every element is delivered");
    assert_eq!(seen.scalars[1], (1, K as i64));
}

/// The `typical` message: seven fields, ids 1..7, one of them a sequence.
#[test]
fn typical_message_carries_its_seven_fields() {
    let wire = typical_wire();
    let mut seen = Seen::default();
    IStream::new()
        .feed(&wire, &mut seen)
        .expect("typical decodes COMPLETE");
    assert_eq!(seen.sequences, 1, "id 7 is a sequence");
    assert_eq!(seen.strings, 1, "id 5 is a string");
    assert_eq!(seen.payload, "sofab".len());
}

/// `blob 1MB` is 1,000,005 bytes on **every** port: a 1-byte header
/// `(1 << 3) | 2`, a 4-byte `fixlen_word` `(1000000 << 3) | 3`, and the payload.
/// The header bytes are checked too, so a port that agreed on the total by
/// accident would still be caught.
#[test]
fn blob_message_has_the_parity_size_and_header() {
    let blob = make_blob();
    assert_eq!(blob.len(), BLOB_LEN);
    assert_eq!(blob[1], K as u8);

    let wire = blob_wire(&blob); // asserts BLOB_SIZE itself
    assert_eq!(wire.len(), BLOB_SIZE);
    assert_eq!(wire[0], (1 << 3) | 2, "field header, id 1, FIXLEN");
    // `fixlen_word = (len << 3) | subtype`, base-128 varint (CORELIB_PLAN §4.6).
    // BENCH_SPEC counts it as 4 bytes, which is where 1,000,005 comes from.
    let mut word = ((BLOB_LEN as u64) << 3) | 3;
    let mut expect = Vec::new();
    while word >= 0x80 {
        expect.push((word as u8 & 0x7F) | 0x80);
        word >>= 7;
    }
    expect.push(word as u8);
    assert_eq!(expect.len(), 4, "the fixlen_word is 4 bytes");
    assert_eq!(&wire[1..5], &expect[..], "fixlen_word = (len << 3) | blob");
    assert_eq!(&wire[5..], &blob[..], "payload follows verbatim");
}

/// The `encode: blob 1MB streaming` row: the same message through a 4096-byte
/// caller buffer with a flush sink, pass-through **not** granted.
///
/// Two things make this row the one BENCH_SPEC actually wants read: every byte
/// passes *through* the buffer (so no flush is wider than it), and the emitted
/// bytes are identical to the one-shot encoding.
#[test]
fn streaming_blob_encode_goes_through_the_buffer() {
    let blob = make_blob();
    let one_shot = blob_wire(&blob);
    let streamed = stream_blob(&blob);

    assert_eq!(
        streamed.bytes, one_shot,
        "streaming and one-shot must emit the same bytes"
    );
    assert!(
        streamed.widest <= BLOB_CHUNK,
        "a flush of {} B is wider than the {BLOB_CHUNK}-byte buffer: the payload \
         reached the sink without passing through it",
        streamed.widest
    );
    assert!(
        streamed.flushes >= BLOB_SIZE / BLOB_CHUNK,
        "{} flush(es) for a {BLOB_SIZE}-byte message through a {BLOB_CHUNK}-byte \
         buffer — the row measures ~245 handovers",
        streamed.flushes
    );
}

/// The `decode: blob 1MB` row: fed in 4096-byte chunks, ending COMPLETE with
/// every payload byte copied out — and copied correctly. A decode that failed on
/// the first chunk would still print a row, and the fastest one in the table.
#[test]
fn chunked_blob_decode_ends_complete() {
    let blob = make_blob();
    let wire = blob_wire(&blob);
    let mut dst = vec![0u8; BLOB_LEN];
    let mut sink = BlobSink::new(&mut dst);
    let last = feed_chunked(&wire, &mut sink);
    let written = sink.written;
    assert!(last.is_ok(), "last chunk ended {last:?}, not COMPLETE");
    assert_eq!(written, BLOB_LEN, "payload bytes delivered");
    assert!(
        dst == blob,
        "payload round-trips through the chunked decode"
    );
}

/// The chunk boundaries are not aligned to anything the encoder produced, so a
/// payload that only survives when a chunk starts at the field header would be
/// caught here: the same wire fed one byte at a time must deliver the same bytes.
#[test]
fn blob_decode_survives_any_chunking() {
    let blob = make_blob();
    let wire = blob_wire(&blob);
    let mut dst = vec![0u8; BLOB_LEN];
    let mut sink = BlobSink::new(&mut dst);
    let mut is = IStream::new();
    // One byte at a time over the header, the length word and the first two
    // payload bytes, then the rest in one go: the payload run has to resume
    // across a feed boundary either way.
    for i in 0..7 {
        let _ = is.feed(&wire[i..i + 1], &mut sink);
    }
    let last = is.feed(&wire[7..], &mut sink);
    assert!(last.is_ok(), "byte-fed decode ended {last:?}, not COMPLETE");
    assert!(dst == blob, "payload round-trips one byte at a time too");
}

/// The `composite` message's encoded size is its cross-port parity check, and
/// the five paths it exists for are all on the wire.
#[test]
fn composite_message_exercises_the_paths_it_was_added_for() {
    let wire = composite_wire(); // asserts COMPOSITE_SIZE itself
    assert_eq!(wire.len(), COMPOSITE_SIZE);

    let mut seen = Seen::default();
    IStream::new()
        .feed(&wire, &mut seen)
        .expect("composite decodes COMPLETE");

    // Field 1's 64 wrapper elements plus field 2's string.
    assert_eq!(seen.strings, COMPOSITE_ELEMENTS as usize + 1);
    // Field 2 is 32 cycles of a 10-byte, four-width UTF-8 string; the wrapper
    // elements are "item-0" ..= "item-63".
    let elements: usize = (0..COMPOSITE_ELEMENTS)
        .map(|i| format!("item-{i}").len())
        .sum();
    assert_eq!(seen.payload, elements + COMPOSITE_TEXT.len() * 32);
    // Fields 1 and 3 and the two sequences nested inside 3 — but *not* field 4,
    // which equals its default and is therefore never framed (MESSAGE_SPEC §2).
    assert_eq!(seen.sequences, 4, "the all-default field 4 is omitted");
    assert_eq!(
        seen.scalars,
        vec![(1, 7), (2, -1), (130, 0xDEAD_BEEF)],
        "the depth-3 nest and the two-byte-header field"
    );
}

/// Field 130 is the suite's only two-byte field header, and the wrapper array's
/// element ids straddle the same boundary — 0..=15 in one byte, 16..=63 in two.
#[test]
fn composite_carries_the_multi_byte_headers() {
    let wire = composite_wire();
    let two_byte: Vec<u8> = {
        let mut buf = [0u8; 8];
        let used = {
            let mut os = OStream::new(&mut buf);
            os.write_unsigned(130, 0xDEAD_BEEF).unwrap();
            os.bytes_used()
        };
        buf[..used].to_vec()
    };
    assert_eq!(
        two_byte[0] & 0x80,
        0x80,
        "(130 << 3) | 0 does not fit one varint byte"
    );
    assert!(
        wire.windows(two_byte.len())
            .any(|w| w == two_byte.as_slice()),
        "the two-byte-header field is on the wire"
    );

    // The wrapper elements straddle the same boundary: element 15's header
    // `(15 << 3) | 2` is the last that fits one varint byte, element 16's
    // `(16 << 3) | 2 = 130` is the first that does not. Each is followed by its
    // `fixlen_word` `(7 << 3) | 2` and the seven bytes of "item-1x".
    let one_byte_element = [&[122u8, 58][..], b"item-15"].concat();
    let two_byte_element = [&[0x82u8, 0x01, 58][..], b"item-16"].concat();
    assert!(
        wire.windows(one_byte_element.len())
            .any(|w| w == one_byte_element.as_slice()),
        "element 15 carries a one-byte header"
    );
    assert!(
        wire.windows(two_byte_element.len())
            .any(|w| w == two_byte_element.as_slice()),
        "element 16 carries a two-byte header"
    );
}

/// `decode: composite skip-all` walks the same bytes with a visitor that
/// overrides nothing: it must still reach the end of the message and report
/// COMPLETE, or the row would be measuring an early exit.
#[test]
fn composite_skip_all_still_walks_the_whole_message() {
    let wire = composite_wire();
    let mut sink = SkipAll;
    IStream::new()
        .feed(&wire, &mut sink)
        .expect("skip-all walks to the end and reports COMPLETE");
}

/// The self-check the bench binary runs before it times anything — asserted here
/// too, so a broken workload fails the test job rather than being noticed (or
/// not) in a benchmark table.
#[test]
fn bench_self_check_passes() {
    let blob = make_blob();
    let blob_wire = blob_wire(&blob);
    let comp_wire = composite_wire();
    self_check(&blob, &blob_wire, &comp_wire);
}
