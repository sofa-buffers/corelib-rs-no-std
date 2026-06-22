//! CPU-cost benchmarks for the sofab encoder/decoder.
//!
//! These use `iai-callgrind`, which runs each benchmark under Valgrind/Callgrind
//! and reports **instruction counts, cache accesses and an estimated cycle
//! count**. Those numbers are *deterministic* and *independent of the host CPU's
//! clock speed* — they measure the cost of the code itself, not how fast this
//! particular machine happens to be. Re-running on a faster/slower box yields
//! the same counts, which is exactly what you want when tracking the footprint
//! and efficiency of an embedded library.
//!
//! Run with:  `cargo bench`
//!
//! Each `setup` function runs *outside* measurement; only the benchmark body is
//! counted.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use sofab::{Id, IStream, OStream, Signed, Unsigned, Visitor};
use std::hint::black_box;

/// Decode sink that folds every value into a checksum so the optimizer cannot
/// elide the decode work.
#[derive(Default)]
struct Checksum {
    acc: u64,
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

/// A spread of unsigned values exercising 1..10-byte varints.
fn make_u64_src(n: usize) -> Vec<u64> {
    (0..n as u64).map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15)).collect()
}

/// Pre-encode an unsigned array message (used as decode input, not measured).
fn make_u64_array_buf(n: usize) -> Vec<u8> {
    let src = make_u64_src(n);
    let mut buf = vec![0u8; n * 11 + 16];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_array_unsigned(1, &src).unwrap();
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// Pre-encode a realistic mixed message (used as decode input, not measured).
fn make_typical_buf() -> Vec<u8> {
    let mut buf = vec![0u8; 256];
    let used = {
        let mut os = OStream::new(&mut buf);
        encode_typical(&mut os);
        os.bytes_used()
    };
    buf.truncate(used);
    buf
}

/// A representative small telemetry-style message: a few scalars, a float, a
/// short string and a small array — plus a nested sequence.
fn encode_typical(os: &mut OStream) {
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_boolean(3, true).unwrap();
    os.write_fp32(4, 3.14159).unwrap();
    os.write_str(5, "sofab").unwrap();
    os.write_array_unsigned(6, &[10u16, 20, 30, 40]).unwrap();
    os.write_sequence_begin(7).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
}

// ----------------------------------------------------------------------------
// Encoding benchmarks
// ----------------------------------------------------------------------------

// Cost of encoding an unsigned array (the varint-encode hot loop).
#[library_benchmark]
#[bench::n1000(args = (1000,), setup = make_u64_src)]
fn encode_u64_array(src: Vec<u64>) -> usize {
    let mut buf = [0u8; 16 * 1024];
    let mut os = OStream::new(&mut buf);
    os.write_array_unsigned(1, black_box(&src)).unwrap();
    black_box(os.bytes_used())
}

// Cost of encoding a realistic mixed message.
#[library_benchmark]
fn encode_typical_message() -> usize {
    let mut buf = [0u8; 256];
    let mut os = OStream::new(&mut buf);
    encode_typical(&mut os);
    black_box(os.bytes_used())
}

// ----------------------------------------------------------------------------
// Decoding benchmarks
// ----------------------------------------------------------------------------

// Cost of decoding an unsigned array (the varint-decode state machine).
#[library_benchmark]
#[bench::n1000(args = (1000,), setup = make_u64_array_buf)]
fn decode_u64_array(buf: Vec<u8>) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(&buf), &mut sink).unwrap();
    black_box(sink.acc)
}

// Cost of decoding a realistic mixed message.
#[library_benchmark]
#[bench::msg(setup = make_typical_buf)]
fn decode_typical_message(buf: Vec<u8>) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(&buf), &mut sink).unwrap();
    black_box(sink.acc)
}

library_benchmark_group!(
    name = encoding;
    benchmarks = encode_u64_array, encode_typical_message
);

library_benchmark_group!(
    name = decoding;
    benchmarks = decode_u64_array, decode_typical_message
);

main!(library_benchmark_groups = encoding, decoding);
