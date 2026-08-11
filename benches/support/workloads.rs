//! The BENCH_SPEC datasets, shared by the bench binary and its tests.
//!
//! BENCH_SPEC defines four datasets — `u64 array (1000)`, the `typical` message,
//! the unbounded `blob 1MB` message and the `composite` message — with the exact
//! field ids and values spelled out, because the encoded bytes are what makes the
//! rows comparable across ports. They live here rather than inside
//! `benches/bench.rs` so that `tests/bench_workloads_tests.rs` can assert them in
//! the ordinary test job: a benchmark row prints a number whatever happens, and
//! the ways these rows degenerate all make them look *faster* (a chunked decode
//! that goes `INVALID` on chunk 1 walks 244 chunks of nothing at a spectacular
//! MB/s; a streaming encode whose sink is never called is just an encode into a
//! 4 KB buffer). CI has no valgrind and nobody reads the numbers there, so the
//! *shape* of each workload is what CI checks.
//!
//! This module is host-side test/bench support: it uses `std` (`Vec`, `String`)
//! freely. The library it drives is the `no_std`, heap-free one, and none of this
//! is compiled into it.

// The float workload value (3.14159) is a fixed payload byte pattern matching
// the C/C++ bench tools, deliberately not `std::f32::consts::PI`; silence the
// approx-constant lint so the cross-language byte comparison stays intact.
#![allow(clippy::approx_constant)]
// Each consumer (bench binary, test crate) uses a subset of the datasets.
#![allow(dead_code)]

use sofab::{Flush, IStream, Id, OStream, Signed, Unsigned, Visitor};
use std::fmt::Write as _;

/// Elements in the `u64 array (1000)` dataset.
pub const N: usize = 1000;

/// The one magic number of the suite: `src[i] = i * K` (wrapping `u64` multiply)
/// generates both the `u64 array (1000)` elements and the `blob 1MB` payload
/// bytes, so the derivation is identical in every language.
pub const K: u64 = 0x9E37_79B9_7F4A_7C15;

/// `blob 1MB` payload length — exactly 1,000,000 bytes, so MB/s reads directly
/// against the `MB = 1e6` convention.
pub const BLOB_LEN: usize = 1_000_000;

/// Encoded size of the `blob 1MB` message, and a cross-port parity check:
/// BENCH_SPEC states it outright, like the `perf` message's 170 bytes — a 1-byte
/// header `(1 << 3) | 2`, a 4-byte `fixlen_word` `(1000000 << 3) | 3`, and the
/// payload.
pub const BLOB_SIZE: usize = 1_000_005;

/// Encoded size of the `composite` message, and its parity check. BENCH_SPEC
/// takes the number from the reference implementation (`corelib-rs`) and then it
/// "must match on every port" — this is that number, asserted here.
pub const COMPOSITE_SIZE: usize = 956;

/// The `blob 1MB` streaming rows are driven through a buffer of exactly this
/// size on every port, so the rows stay comparable across languages. It is
/// deliberately *not* this port's own minimum: [`sofab::MIN_OUTPUT_BUFFER`] is 1
/// here, and a row measured through a one-byte window would compare with nothing.
pub const BLOB_CHUNK: usize = 4096;

/// One cycle of the `composite` string field: 1-, 2-, 3- and 4-byte UTF-8.
pub const COMPOSITE_TEXT: &str = "a\u{e4}\u{20ac}\u{1d11e}";

/// Elements in the `composite` wrapper array (field 1).
pub const COMPOSITE_ELEMENTS: u32 = 64;

// ---------------------------------------------------------------------------
// datasets
// ---------------------------------------------------------------------------

/// A spread of unsigned values exercising 1..10-byte varints.
pub fn make_src() -> Vec<u64> {
    (0..N as u64).map(|i| i.wrapping_mul(K)).collect()
}

/// Payload of the `blob 1MB` dataset — the low byte of the same generator.
pub fn make_blob() -> Vec<u8> {
    (0..BLOB_LEN as u64)
        .map(|i| i.wrapping_mul(K) as u8)
        .collect()
}

/// A representative small telemetry-style message: a few scalars, a float, a
/// short string and a small array — plus a nested sequence.
pub fn encode_typical(os: &mut OStream) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_boolean(3, true).unwrap();
    os.write_fp32(4, 3.14159).unwrap();
    os.write_str(5, "sofab").unwrap();
    os.write_array_unsigned(6, &[10u16, 20, 30, 40]).unwrap();
    os.write_sequence_begin_lazy(7).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

/// The `composite` message (BENCH_SPEC): every encoder path the flat datasets
/// miss.
///
/// * id 1 — the suite's only **wrapper array** (MESSAGE_SPEC §5.1): one field
///   header per element, element id = array index, so ids 0–15 take a one-byte
///   header and 16–63 take two.
/// * id 2 — 320 UTF-8 bytes covering 1-, 2-, 3- and 4-byte sequences.
/// * id 3 — nesting at **depth 3**, so the held-back run grows past the single
///   level `typical` and `perf` reach. It stays well inside this port's
///   [`sofab::LAZY_SEQ_DEPTH`] window of 8, so the row measures the hold-back
///   rather than the deep-nesting fallback.
/// * id 4 — equal to its declared default, so the encoder must **not** write it:
///   opened lazily, closed with the field closer, gone from the wire. This is the
///   hold-back's discard path.
/// * id 130 — the suite's only **two-byte field header**, `(130 << 3) | 0`.
pub fn encode_composite(os: &mut OStream) {
    // id 1: wrapper array of 64 strings, "item-0" ..= "item-63".
    os.write_sequence_begin_lazy(1).unwrap();
    let mut element = String::new();
    for i in 0..COMPOSITE_ELEMENTS {
        element.clear();
        element.push_str("item-");
        write!(element, "{i}").unwrap();
        os.write_str(i, &element).unwrap();
    }
    os.write_sequence_end().unwrap();

    // id 2: 32 repetitions of a 10-byte, four-width UTF-8 cycle.
    os.write_str(2, &COMPOSITE_TEXT.repeat(32)).unwrap();

    // id 3: { 1: { 1: { 1: unsigned 7 } }, 2: signed -1 }
    os.write_sequence_begin_lazy(3).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_sequence_begin_lazy(1).unwrap();
    os.write_unsigned(1, 7).unwrap();
    os.write_sequence_end().unwrap();
    os.write_sequence_end().unwrap();
    os.write_signed(2, -1).unwrap();
    os.write_sequence_end().unwrap();

    // id 4: all-default struct — opened and dropped, emitting nothing.
    os.write_sequence_begin_lazy(4).unwrap();
    os.write_sequence_end().unwrap();

    // id 130: the two-byte header.
    os.write_unsigned(130, 0xDEAD_BEEF).unwrap();
}

// ---------------------------------------------------------------------------
// pre-encoded wires (the decode inputs, and the parity checks)
// ---------------------------------------------------------------------------

/// `u64 array (1000)` on the wire.
pub fn u64_array_wire(src: &[u64]) -> Vec<u8> {
    let mut buf = vec![0u8; N * 11 + 16];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_array_unsigned(1, src).unwrap();
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// The `typical` message on the wire.
pub fn typical_wire() -> Vec<u8> {
    let mut buf = vec![0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        encode_typical(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// The `blob 1MB` message on the wire, encoded the way the one-shot row is:
/// a caller buffer of exactly [`BLOB_SIZE`] bytes and **no sink**.
///
/// The buffer is sized by hand, as BENCH_SPEC requires: the schema is declared
/// without `maxlen`, so no generated `MAX_SIZE` bounds it.
pub fn blob_wire(blob: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; BLOB_SIZE];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_blob(1, blob).unwrap();
        os.bytes_used()
    };
    assert_eq!(
        used, BLOB_SIZE,
        "the blob 1MB encoded size is a cross-port parity check"
    );
    buf.truncate(used);
    buf
}

/// The `composite` message on the wire.
pub fn composite_wire() -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    let used = {
        let mut os = OStream::new(&mut buf);
        encode_composite(&mut os);
        os.bytes_used()
    };
    assert_eq!(
        used, COMPOSITE_SIZE,
        "the composite encoded size is the parity check every port compares \
         itself against"
    );
    buf.truncate(used);
    buf
}

// ---------------------------------------------------------------------------
// sinks
// ---------------------------------------------------------------------------

/// The measured streaming row's sink. BENCH_SPEC is explicit that it **consumes
/// and discards**: accumulating would add to the streaming row a copy the
/// one-shot row never pays, and I/O is not deterministic under Callgrind. Folding
/// one byte per call is the minimum that keeps the call from being optimised
/// away.
///
/// It installs no replacement buffer, so it is the **copying** half of the §5.1
/// returning-callback contract: the encoder keeps the caller's buffer and resumes
/// at offset 0. That is the path the row is meant to measure.
#[derive(Default)]
pub struct Discard {
    pub acc: u8,
}

impl Flush for Discard {
    fn flush(&mut self, data: &[u8]) {
        self.acc ^= data.first().copied().unwrap_or(0);
    }
}

/// Decode sink that folds every value into a checksum so the optimizer cannot
/// elide the decode work.
#[derive(Default)]
pub struct Checksum {
    pub acc: u64,
}

impl Visitor for Checksum {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.acc = self.acc.wrapping_add(v ^ id as u64);
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.acc = self.acc.wrapping_add((v as u64) ^ id as u64);
    }
    fn fp32(&mut self, _id: Id, v: f32) {
        self.acc = self.acc.wrapping_add(v.to_bits() as u64);
    }
    fn fp64(&mut self, _id: Id, v: f64) {
        self.acc = self.acc.wrapping_add(v.to_bits());
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
}

/// Destination of the `decode: blob 1MB` row: the payload is **copied out**,
/// chunk by chunk, into a caller buffer.
///
/// Not a length-counting sink, deliberately. This decoder hands the visitor a
/// borrowed slice of the fed chunk and copies nothing itself, so a visitor that
/// only adds `chunk.len()` leaves a megabyte-sized row measuring 245 slice
/// hand-offs — it reports hundreds of GB/s and moves when the framing changes,
/// which is not what a `blob` row is for. A consumer that wants the bytes must
/// take them somewhere, and that is what the generated `no_std` code does (into a
/// fixed-size array), so the row does it too. It matches the C++ port's row,
/// which reads the payload into a destination for the same reason.
pub struct BlobSink<'a> {
    pub dst: &'a mut [u8],
    /// Payload bytes copied so far — the field the self-check reads.
    pub written: usize,
}

impl<'a> BlobSink<'a> {
    pub fn new(dst: &'a mut [u8]) -> Self {
        BlobSink { dst, written: 0 }
    }
}

impl Visitor for BlobSink<'_> {
    fn blob(&mut self, _id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        let end = (offset + chunk.len()).min(self.dst.len());
        if offset < end {
            self.dst[offset..end].copy_from_slice(&chunk[..end - offset]);
            self.written = end;
        }
    }
}

/// The `decode: composite skip-all` destination: a visitor that overrides no
/// callback, so the decoder walks every header, count and payload length and
/// nothing is read into anything. In a push/visitor port that *is* the skip path
/// (MESSAGE_SPEC §7.2 item 7); its distance from `decode: composite` is what
/// not-decoding is worth.
pub struct SkipAll;
impl Visitor for SkipAll {}

/// What a decode actually delivered — the counters the self-check reads.
#[derive(Default)]
pub struct Seen {
    pub payload: usize,
    pub strings: usize,
    pub sequences: usize,
    pub scalars: Vec<(Id, i64)>,
}

impl Visitor for Seen {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.scalars.push((id, v as i64));
    }
    fn signed(&mut self, id: Id, v: Signed) {
        self.scalars.push((id, v));
    }
    fn string(&mut self, _id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        self.strings += usize::from(offset == 0);
        self.payload += chunk.len();
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.payload += chunk.len();
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.sequences += 1;
    }
}

// ---------------------------------------------------------------------------
// the checks
// ---------------------------------------------------------------------------

/// What the `encode: blob 1MB streaming` row actually did: the bytes that
/// reached the sink, how many handovers it took, and the widest one.
pub struct Streamed {
    pub bytes: Vec<u8>,
    pub flushes: usize,
    pub widest: usize,
}

/// Drive the streaming row's encode with a **recording** sink: exactly the row's
/// setup — a caller buffer of [`BLOB_CHUNK`] bytes with a flush sink, no
/// pass-through — but keeping what it emitted so the caller can check it.
///
/// The measured row uses [`Discard`] instead; this one is only ever run outside a
/// timed loop and outside the Callgrind toggle.
pub fn stream_blob(blob: &[u8]) -> Streamed {
    let mut scratch = [0u8; BLOB_CHUNK];
    let mut out = Streamed {
        bytes: Vec::with_capacity(BLOB_SIZE),
        flushes: 0,
        widest: 0,
    };
    {
        let mut os = OStream::with_flush(&mut scratch, 0, |data: &[u8]| {
            out.flushes += 1;
            out.widest = out.widest.max(data.len());
            out.bytes.extend_from_slice(data);
        })
        .expect("4096 bytes clears MIN_OUTPUT_BUFFER");
        os.write_blob(1, blob).unwrap();
        os.flush();
    }
    out
}

/// Feed `wire` to a fresh decoder in [`BLOB_CHUNK`]-byte chunks, as the
/// `decode: blob 1MB` row does, and return the outcome of the **last** chunk.
///
/// Every chunk but the last leaves the decode INCOMPLETE, which is an outcome
/// and not an error (CORELIB_PLAN §5.2) — only the last one is expected to be
/// COMPLETE, and that is the caller's assertion to make.
pub fn feed_chunked<V: Visitor>(wire: &[u8], visitor: &mut V) -> sofab::Result<()> {
    let mut is = IStream::new();
    let mut last = Err(sofab::Error::Incomplete);
    for chunk in wire.chunks(BLOB_CHUNK) {
        last = is.feed(chunk, visitor);
    }
    last
}

/// Prove each workload does the work its row is named after.
///
/// Run once from the bench binary before anything is timed, and asserted again
/// from `tests/bench_workloads_tests.rs` in CI. Cost is one extra op of each; it
/// is outside every measured loop and outside the Callgrind toggle.
pub fn self_check(blob: &[u8], blob_wire: &[u8], comp_wire: &[u8]) {
    // `encode: blob 1MB streaming` — the whole message reaches the sink, in
    // buffer-sized pieces, and the bytes are the one-shot encoding. A piece
    // wider than the buffer would mean the payload reached the sink without
    // passing through it, which is the pass-through path this row is
    // specifically required *not* to take.
    let streamed = stream_blob(blob);
    assert_eq!(
        streamed.bytes.len(),
        BLOB_SIZE,
        "streaming encode: bytes reaching the sink"
    );
    assert!(
        streamed.bytes == blob_wire,
        "streaming encode: the bytes through a {BLOB_CHUNK}-byte window must be \
         the one-shot encoding"
    );
    assert!(
        streamed.flushes >= BLOB_SIZE / BLOB_CHUNK && streamed.widest <= BLOB_CHUNK,
        "streaming encode: {} flush(es), widest {} B — the row measures ~245 \
         buffer-sized handovers, not one big one",
        streamed.flushes,
        streamed.widest
    );

    // `decode: blob 1MB` — fed in 4096-byte chunks, ending COMPLETE with every
    // payload byte copied out, and copied *correctly*: a row that decoded the
    // framing and dropped the payload would be the fastest one in the table.
    let mut dst = vec![0u8; BLOB_LEN];
    let mut sink = BlobSink::new(&mut dst);
    let last = feed_chunked(blob_wire, &mut sink);
    let written = sink.written;
    assert!(
        last.is_ok(),
        "chunked blob decode ended {last:?}, not COMPLETE"
    );
    assert_eq!(written, BLOB_LEN, "chunked blob decode: bytes delivered");
    assert!(dst == blob, "chunked blob decode: payload round-trips");

    // `composite` — the five paths it was added for: 64 wrapper elements plus
    // the UTF-8 string, four sequences on the wire (field 4 omitted, not
    // framed), and the scalars from the depth-3 nest and the two-byte header.
    let mut seen = Seen::default();
    IStream::new()
        .feed(comp_wire, &mut seen)
        .expect("composite decodes COMPLETE");
    assert_eq!(seen.strings, 65, "composite: wrapper elements + string");
    assert_eq!(
        seen.sequences, 4,
        "composite: the all-default field 4 is omitted"
    );
    assert_eq!(
        seen.scalars,
        vec![(1, 7), (2, -1), (130, 0xDEAD_BEEF)],
        "composite: depth-3 nest and the two-byte-header field"
    );
}
