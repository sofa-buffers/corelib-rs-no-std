//! Wall-clock **throughput** speedtest for the sofab encoder/decoder.
//!
//! This is the machine-*dependent* counterpart to `perf.rs`. Where `perf.rs`
//! uses iai-callgrind to report deterministic instruction counts that are
//! independent of the host CPU, this benchmark times the real code on *this*
//! machine and reports results in **MB/s** (1 MB = 1_000_000 bytes). The
//! numbers vary with CPU speed, load and build flags — that is exactly the
//! point: it answers "how fast does the implementation actually run here?".
//!
//! It is a plain binary (`harness = false`), so it needs no Valgrind and no
//! special privileges — unlike `perf.rs` it runs anywhere `cargo` does.
//!
//! Run with:
//!   cargo bench --bench bench
//!   BENCH_MS=2000 cargo bench --bench bench   # longer/steadier measurement

use sofab::{Id, IStream, OStream, Signed, Unsigned, Visitor};
use std::hint::black_box;
use std::time::{Duration, Instant};

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

/// Pre-encode an unsigned array message (used as decode input).
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

/// Pre-encode the typical message (used as decode input).
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

/// Run `body` repeatedly for at least `budget` (after a short warm-up) and
/// return how many iterations completed and the elapsed time. The clock is
/// only read once per batch so timer overhead stays off the hot path.
fn time_loop(budget: Duration, mut body: impl FnMut()) -> (u64, Duration) {
    // Warm up (~10% of the budget) so caches and branch predictors are hot.
    let warm = Instant::now();
    while warm.elapsed() < budget / 10 {
        body();
    }

    const BATCH: u64 = 256;
    let start = Instant::now();
    let mut iters = 0u64;
    loop {
        for _ in 0..BATCH {
            body();
        }
        iters += BATCH;
        if start.elapsed() >= budget {
            break;
        }
    }
    (iters, start.elapsed())
}

/// Print one result line. `bytes_per_iter` is the size of the wire message the
/// operation produces (encode) or consumes (decode); throughput is reported
/// against that volume.
fn report(name: &str, bytes_per_iter: usize, iters: u64, elapsed: Duration) {
    let total = bytes_per_iter as f64 * iters as f64;
    let secs = elapsed.as_secs_f64();
    let mb_s = total / secs / 1.0e6;
    let ns_per_op = secs * 1.0e9 / iters as f64;
    println!(
        "{name:<26} {mb_s:>9.1} MB/s   ({bytes_per_iter:>4} B/op, {ns_per_op:>8.1} ns/op, {iters} ops)"
    );
}

fn main() {
    let budget = Duration::from_millis(
        std::env::var("BENCH_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(1000),
    );

    println!(
        "sofab throughput speedtest  (MB = 1_000_000 bytes, {} ms/bench)\n",
        budget.as_millis()
    );
    println!("{:<26} {:>9}        details", "benchmark", "throughput");
    println!("{}", "-".repeat(74));

    // --- encode: unsigned array (varint-encode hot loop) -------------------
    {
        let src = make_u64_src(1000);
        let mut buf = vec![0u8; 1000 * 11 + 16];
        let out_len = {
            let mut os = OStream::new(&mut buf);
            os.write_array_unsigned(1, &src).unwrap();
            os.bytes_used()
        };
        let (iters, el) = time_loop(budget, || {
            let used = {
                let mut os = OStream::new(&mut buf);
                os.write_array_unsigned(1, black_box(&src)).unwrap();
                os.bytes_used()
            };
            black_box(&buf[..used]);
        });
        report("encode_u64_array[1000]", out_len, iters, el);
    }

    // --- decode: unsigned array (varint-decode state machine) --------------
    {
        let enc = make_u64_array_buf(1000);
        let len = enc.len();
        let (iters, el) = time_loop(budget, || {
            let mut sink = Checksum::default();
            let mut is = IStream::new();
            is.feed(black_box(&enc), &mut sink).unwrap();
            black_box(sink.acc);
        });
        report("decode_u64_array[1000]", len, iters, el);
    }

    // --- encode: realistic mixed message -----------------------------------
    {
        let mut buf = [0u8; 256];
        let out_len = {
            let mut os = OStream::new(&mut buf);
            encode_typical(&mut os);
            os.bytes_used()
        };
        let (iters, el) = time_loop(budget, || {
            let used = {
                let mut os = OStream::new(&mut buf);
                encode_typical(&mut os);
                os.bytes_used()
            };
            black_box(&buf[..used]);
        });
        report("encode_typical_message", out_len, iters, el);
    }

    // --- decode: realistic mixed message -----------------------------------
    {
        let enc = make_typical_buf();
        let len = enc.len();
        let (iters, el) = time_loop(budget, || {
            let mut sink = Checksum::default();
            let mut is = IStream::new();
            is.feed(black_box(&enc), &mut sink).unwrap();
            black_box(sink.acc);
        });
        report("decode_typical_message", len, iters, el);
    }
}
