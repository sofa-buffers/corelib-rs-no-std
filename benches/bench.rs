//! SofaBuffers Rust (no_std) — throughput benchmark (MB/s, CPU time).
//!
//! Mirror of `bench/c/bench.c` and `bench/cpp/bench.cpp`: encode/decode
//! throughput for the four BENCH_SPEC datasets — a 1000-element u64 array, a
//! small "typical" mixed message, an unbounded 1 MB `blob`, and the `composite`
//! message that exercises the paths the flat three never reach (wrapper array,
//! multi-byte UTF-8, depth-3 nesting, an omitted default, a two-byte field
//! header). Each workload runs in a ~1 s loop and reports MB/s, and the output
//! table matches the C/C++ tools so the implementations can be compared directly.
//!
//! The datasets themselves live in `benches/support/workloads.rs`, shared with
//! `tests/bench_workloads_tests.rs`, which asserts in CI that each row does the
//! work its name claims.
//!
//! **The `blob 1MB` rows are not a statement about this port's speed.** Five
//! bytes of that message are metadata and a million are payload, so its MB/s is
//! dominated by how this encoder moves payload bytes and by the machine's memory
//! bandwidth; the figure does not belong next to `typical message`. BENCH_SPEC
//! puts the signal in the *difference* between the one-shot and streaming rows —
//! the cost of the divisible-run path (CORELIB_PLAN §5.1) — and points at
//! Callgrind `Ir/op` (`benches/run_callgrind.sh`) to read it, since instruction
//! counts do not care about bandwidth. On this port the two readings disagree
//! sharply, and the instruction count is the honest one: every payload byte goes
//! through the same one-byte push primitive either way (which is what lets
//! `MIN_OUTPUT_BUFFER` be 1), but with no sink that loop has a single exit
//! condition and LLVM turns it into a `memcpy` — ~0.2 Ir/byte against ~11 for the
//! streaming loop, whose per-byte "is the buffer full?" test keeps it
//! byte-at-a-time. In MB/s the one-shot row gives most of that back, because at
//! ~2 GB/s it is bandwidth-bound and the streaming row, working inside a
//! cache-resident 4 KB window, is not.
//!
//! Throughput is measured against *process CPU time* (`clock()`, not
//! wall-clock), so the number reflects the cost of the implementation rather
//! than OS scheduling noise or the wall-clock speed of the host. MB = 1e6 bytes.
//!
//! Run with:  `cargo bench --bench bench`

use sofab::{IStream, OStream};
use std::hint::black_box;

#[path = "support/workload_arg.rs"]
mod workload_arg;
use workload_arg::workload_arg;

#[path = "support/workloads.rs"]
mod workloads;
use workloads::{
    blob_wire, composite_wire, encode_composite, encode_typical, make_blob, make_src, self_check,
    typical_wire, u64_array_wire, BlobSink, Checksum, Discard, SkipAll, BLOB_CHUNK, BLOB_LEN,
    BLOB_SIZE, N,
};

/// Process CPU time in seconds (not wall-clock), via
/// `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)` — the higher-resolution equivalent
/// of the C tool's `clock()`.
fn cpu_now() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid, writable timespec; the clock id is valid on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

// ---- Callgrind workload entry points --------------------------------------
// Each function performs *exactly one* operation and is `#[inline(never)]` +
// `#[unsafe(no_mangle)]`, so `bench/run_callgrind.sh` can run
//   valgrind --tool=callgrind --collect-atstart=no --toggle-collect=run_<w>
// and collect the instructions retired (Ir) for a single op — a deterministic,
// machine-independent per-op cost. `black_box` keeps the op from being elided
// or const-folded. Setup (encoding the decode inputs) happens in `main` before
// the call, so it stays outside the collected region.

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_u64_array(src: &[u64], out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    os.write_array_unsigned(1, black_box(src)).unwrap();
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_typical(out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    encode_typical(&mut os);
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_u64_array(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_typical(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

/// `encode: blob 1MB one-shot` — the floor: one contiguous write into a caller
/// buffer of exactly [`BLOB_SIZE`] bytes, **no sink**, so no flush logic runs.
#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_blob_oneshot(blob: &[u8], out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    os.write_blob(1, black_box(blob)).unwrap();
    black_box(os.bytes_used())
}

/// `encode: blob 1MB streaming` — the same bytes through a caller buffer of
/// exactly [`BLOB_CHUNK`] bytes with a flush sink that consumes and discards, so
/// the megabyte crosses the buffer ~245 times. Pass-through is not granted (this
/// port implements none), so this is the copy path on purpose.
#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_blob_streaming(blob: &[u8], scratch: &mut [u8]) -> usize {
    let mut os = OStream::with_flush(scratch, 0, Discard::default())
        .expect("4096 bytes clears MIN_OUTPUT_BUFFER");
    os.write_blob(1, black_box(blob)).unwrap();
    black_box(os.flush())
}

/// `decode: blob 1MB` — fed in [`BLOB_CHUNK`]-byte chunks, with the payload
/// copied into `dst` (see [`BlobSink`]: this decoder lends the visitor a slice of
/// the fed chunk, so a sink that only counted lengths would leave the row
/// measuring nothing). Every chunk but the last leaves the decode INCOMPLETE,
/// which is an outcome and not an error (CORELIB_PLAN §5.2); `self_check` is
/// where the last one is required to be COMPLETE and the payload required to have
/// arrived intact.
#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_blob(wire: &[u8], dst: &mut [u8]) -> usize {
    let mut sink = BlobSink::new(dst);
    let mut is = IStream::new();
    for chunk in black_box(wire).chunks(BLOB_CHUNK) {
        let _ = is.feed(chunk, &mut sink);
    }
    black_box(sink.written)
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_encode_composite(out: &mut [u8]) -> usize {
    let mut os = OStream::new(out);
    encode_composite(&mut os);
    black_box(os.bytes_used())
}

#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_composite(wire: &[u8]) -> u64 {
    let mut sink = Checksum::default();
    let mut is = IStream::new();
    is.feed(black_box(wire), &mut sink).unwrap();
    black_box(sink.acc)
}

/// `decode: composite skip-all` — walk the message, materialize nothing.
///
/// In a push/visitor port that is a visitor which overrides no callback: the
/// decoder still walks every header, count and payload length, but nothing is
/// read into a destination. Its distance from `run_decode_composite` is what
/// not-decoding is worth here.
#[inline(never)]
#[unsafe(no_mangle)]
pub fn run_decode_composite_skip(wire: &[u8]) -> bool {
    let mut sink = SkipAll;
    let mut is = IStream::new();
    black_box(is.feed(black_box(wire), &mut sink).is_ok())
}

/// How long one batch of operations runs before the clock is read again.
///
/// `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)` is a real syscall — never
/// vDSO-accelerated — costing on the order of a microsecond. Reading it once per
/// iteration times the clock rather than the codec: the typical-message
/// workloads are tens of nanoseconds per op, so the measurement was ~97 % of
/// what they reported. Ten milliseconds of work per read puts it below 0.01 %.
const BATCH_SECS: f64 = 0.01;

/// Run `body` repeatedly until ~1 s of CPU time has elapsed (after one warm-up
/// call) and return throughput in MB/s for a message of `bytes` bytes.
///
/// The clock is read once per batch, never per operation — see [`BATCH_SECS`].
fn measure(bytes: usize, mut body: impl FnMut()) -> f64 {
    body(); // warmup

    // Grow a batch until it spans BATCH_SECS, so the clock read that ends it is
    // a rounding error against the work it timed.
    let mut batch: u64 = 1;
    loop {
        let t0 = cpu_now();
        for _ in 0..batch {
            body();
        }
        if cpu_now() - t0 >= BATCH_SECS {
            break;
        }
        batch = batch.saturating_mul(2);
    }

    let t0 = cpu_now();
    let mut it: u64 = 0;
    let mut el;
    loop {
        for _ in 0..batch {
            body();
        }
        it += batch;
        el = cpu_now() - t0;
        if el >= 1.0 {
            break;
        }
    }
    bytes as f64 * it as f64 / el / 1e6 // MB/s, MB = 1e6 bytes
}

fn main() {
    // Pre-encode the messages (to learn their byte sizes and as decode input).
    // `blob_wire` and `composite_wire` assert their cross-port parity sizes.
    let src = make_src();
    let u64_buf = u64_array_wire(&src);
    let typ_buf = typical_wire();
    let blob = make_blob();
    let blob_buf = blob_wire(&blob);
    let comp_buf = composite_wire();

    self_check(&blob, &blob_buf, &comp_buf);

    let ba = u64_buf.len();
    let bt = typ_buf.len();
    let bb = blob_buf.len();
    let bc = comp_buf.len();

    // Callgrind mode: `bench <workload>` performs exactly one op of <workload>
    // and exits, so run_callgrind.sh can toggle collection around the run_*
    // symbol. `BYTES=<n>` on stderr feeds the table's size column. The decode
    // inputs were encoded above — outside the collected op.
    // Cargo appends its own `--bench` when it runs this target, so flags are
    // skipped when looking for the workload — see [`workload_arg`].
    if let Some(w) = workload_arg(std::env::args()) {
        let mut enc_u64_out = vec![0u8; N * 11 + 16];
        let mut enc_typ_out = [0u8; 256];
        let mut enc_blob_out = vec![0u8; BLOB_SIZE];
        let mut enc_blob_scratch = vec![0u8; BLOB_CHUNK];
        let mut enc_comp_out = vec![0u8; bc];
        let bytes = match w.as_str() {
            "encode_u64_array" => run_encode_u64_array(&src, &mut enc_u64_out),
            "encode_typical" => run_encode_typical(&mut enc_typ_out),
            "encode_blob_oneshot" => run_encode_blob_oneshot(&blob, &mut enc_blob_out),
            "encode_blob_streaming" => {
                run_encode_blob_streaming(&blob, &mut enc_blob_scratch);
                bb
            }
            "encode_composite" => run_encode_composite(&mut enc_comp_out),
            "decode_u64_array" => {
                run_decode_u64_array(&u64_buf);
                ba
            }
            "decode_typical" => {
                run_decode_typical(&typ_buf);
                bt
            }
            "decode_blob" => {
                let mut dst = vec![0u8; BLOB_LEN];
                run_decode_blob(&blob_buf, &mut dst);
                bb
            }
            "decode_composite" => {
                run_decode_composite(&comp_buf);
                bc
            }
            "decode_composite_skip" => {
                run_decode_composite_skip(&comp_buf);
                bc
            }
            other => {
                eprintln!("unknown workload: {other}");
                std::process::exit(2);
            }
        };
        eprintln!("BYTES={bytes}");
        return;
    }

    // Encode targets (reused across iterations; allocation is outside the loop).
    let mut enc_u64_out = vec![0u8; N * 11 + 16];
    let mut enc_typ_out = [0u8; 256];

    let enc_u64 = measure(ba, || {
        let mut os = OStream::new(&mut enc_u64_out);
        os.write_array_unsigned(1, black_box(&src)).unwrap();
        black_box(os.bytes_used());
    });
    let enc_typ = measure(bt, || {
        let mut os = OStream::new(&mut enc_typ_out);
        encode_typical(&mut os);
        black_box(os.bytes_used());
    });
    let dec_u64 = measure(ba, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&u64_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });
    let dec_typ = measure(bt, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&typ_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });

    // `blob 1MB`: the one-shot row is the floor — one contiguous write, no flush
    // logic — and the streaming row is the same bytes through ~245 flushes into a
    // 4096-byte buffer. Their difference is the divisible-run path.
    let mut enc_blob_out = vec![0u8; BLOB_SIZE];
    let mut enc_blob_scratch = vec![0u8; BLOB_CHUNK];
    let enc_blob_1 = measure(bb, || {
        let mut os = OStream::new(&mut enc_blob_out);
        os.write_blob(1, black_box(&blob)).unwrap();
        black_box(os.bytes_used());
    });
    let enc_blob_s = measure(bb, || {
        let mut os = OStream::with_flush(&mut enc_blob_scratch, 0, Discard::default())
            .expect("4096 bytes clears MIN_OUTPUT_BUFFER");
        os.write_blob(1, black_box(&blob)).unwrap();
        black_box(os.flush());
    });
    let mut dec_blob_dst = vec![0u8; BLOB_LEN];
    let dec_blob = measure(bb, || {
        let mut sink = BlobSink::new(&mut dec_blob_dst);
        let mut is = IStream::new();
        for chunk in black_box(&blob_buf).chunks(BLOB_CHUNK) {
            let _ = is.feed(chunk, &mut sink);
        }
        black_box(sink.written);
    });

    let mut enc_comp_out = vec![0u8; bc];
    let enc_comp = measure(bc, || {
        let mut os = OStream::new(&mut enc_comp_out);
        encode_composite(&mut os);
        black_box(os.bytes_used());
    });
    let dec_comp = measure(bc, || {
        let mut sink = Checksum::default();
        let mut is = IStream::new();
        is.feed(black_box(&comp_buf), &mut sink).unwrap();
        black_box(sink.acc);
    });
    // Written out rather than calling `run_decode_composite_skip`: that entry
    // point is `#[inline(never)]` for Callgrind's sake, and routing one row of
    // the table through a call the other rows do not pay would put the
    // difference this row exists to show — decode versus skip — on the wrong
    // side of a function call.
    let dec_comp_skip = measure(bc, || {
        let mut sink = SkipAll;
        let mut is = IStream::new();
        is.feed(black_box(&comp_buf), &mut sink).unwrap();
    });

    println!("=== SofaBuffers Rust (no_std) throughput (CPU time, MB/s) ===");
    println!("{:<26} {:>12}", "Workload", "MB/s");
    println!("{:<26} {:>12}", "--------", "----");
    println!("{:<26} {:>12.2}", "encode: u64 array (1000)", enc_u64);
    println!("{:<26} {:>12.2}", "encode: typical message", enc_typ);
    println!("{:<26} {:>12.2}", "encode: blob 1MB one-shot", enc_blob_1);
    println!("{:<26} {:>12.2}", "encode: blob 1MB streaming", enc_blob_s);
    // `encode: blob 1MB passthrough` is BENCH_SPEC's one optional row and this
    // port implements no pass-through (CORELIB_PLAN §5.1 makes it a MAY), so the
    // row is omitted entirely rather than printed as a placeholder.
    println!("{:<26} {:>12.2}", "encode: composite", enc_comp);
    println!("{:<26} {:>12.2}", "decode: u64 array (1000)", dec_u64);
    println!("{:<26} {:>12.2}", "decode: typical message", dec_typ);
    println!("{:<26} {:>12.2}", "decode: blob 1MB", dec_blob);
    println!("{:<26} {:>12.2}", "decode: composite", dec_comp);
    println!(
        "{:<26} {:>12.2}",
        "decode: composite skip-all", dec_comp_skip
    );
    println!("\nMB = 1e6 bytes. ~1s CPU-time loop per workload.");
    println!("blob 1MB is bandwidth-bound: compare it with the same row on another port.");
}
