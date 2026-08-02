//! SofaBuffers Rust — combined per-operation cost benchmark.
//!
//! Mirror of `bench/c/perf.c` and `bench/cpp/perf.cpp`: encodes/decodes the
//! identical message (same field ids, types and values) through the streaming
//! API and prints the identical report, so the C, C++ and Rust implementations
//! can be compared directly. Two complementary metrics per workload:
//!
//!   1. CPU cycles/op  -- cost of the code itself, read off the hardware cycle
//!      counter (x86 TSC via `_rdtsc`, AArch64 virtual count register). Tracks
//!      code changes rather than the host's clock speed.
//!
//!   2. Throughput MB/s -- a "speedtest" for this machine, derived from process
//!      CPU time (`clock()`, not wall-clock). MB = 1e6 bytes.
//!
//! Both metrics are gathered over the same adaptive ~1 s CPU-time loop, so they
//! describe the exact same work.
//!
//! Run with:  `cargo bench --bench perf`

// The float workload values (3.14159…, 6.28318…, e, …) are fixed payload bytes
// chosen to match the C/C++ bench tools exactly. They are deliberately *not*
// `std::f64::consts::{PI,TAU,E}` — using those would change the encoded bytes
// and break cross-language comparison — so silence clippy's approx-constant lint.
#![allow(clippy::approx_constant)]

use sofab::{IStream, Id, OStream, Signed, Unsigned, Visitor};
use std::hint::black_box;

// ---------------------------------------------------------------------------
// hardware cycle counter (same idea as the C/C++ benchmark)
// ---------------------------------------------------------------------------
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod cycles {
    pub const HAVE: bool = true;
    #[inline]
    pub fn read() -> u64 {
        #[cfg(target_arch = "x86")]
        use core::arch::x86::_rdtsc;
        #[cfg(target_arch = "x86_64")]
        use core::arch::x86_64::_rdtsc;
        // SAFETY: rdtsc is part of the baseline x86/x86_64 instruction set.
        unsafe { _rdtsc() }
    }
}
#[cfg(target_arch = "aarch64")]
mod cycles {
    pub const HAVE: bool = true;
    #[inline]
    pub fn read() -> u64 {
        let v: u64;
        // SAFETY: cntvct_el0 is readable from EL0 on AArch64.
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v) };
        v
    }
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
mod cycles {
    pub const HAVE: bool = false;
    pub fn read() -> u64 {
        0
    }
}

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

// ---------------------------------------------------------------------------
// message under test (identical to perf.c / perf.cpp)
// ---------------------------------------------------------------------------
const PERF_STRING: &str = "perf-benchmark-message";

const PERF_SAMPLES: [u32; 8] = [
    1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000, 8_000_000,
];
const PERF_DELTAS: [i32; 8] = [
    -100_000, -200_000, -300_000, -400_000, -500_000, -600_000, -700_000, -800_000,
];
const PERF_FP64: [f64; 4] = [3.14159265, 6.28318530, 9.42477795, 12.56637060];

fn perf_encode(buf: &mut [u8]) -> usize {
    let mut os = OStream::new(buf);
    os.write_unsigned(1, 0xDEAD_BEEF).unwrap();
    os.write_signed(2, -12345).unwrap();
    os.write_unsigned(3, 0x0123_4567_89AB_CDEF).unwrap();
    os.write_signed(4, -5_000_000_000_000).unwrap();
    os.write_boolean(5, true).unwrap();
    os.write_fp32(6, 3.14159).unwrap();
    os.write_fp64(7, 2.718281828459045).unwrap();
    os.write_str(8, PERF_STRING).unwrap();
    os.write_array_unsigned(9, &PERF_SAMPLES).unwrap();
    os.write_array_signed(10, &PERF_DELTAS).unwrap();
    os.write_array_fp64(11, &PERF_FP64).unwrap();
    os.write_sequence_begin_lazy(12).unwrap();
    os.write_unsigned(1, 99).unwrap();
    os.write_signed(2, -7).unwrap();
    os.write_sequence_end().unwrap();
    os.bytes_used()
}

/// Decode sink: folds every value into a checksum (so nothing is elided) and
/// captures the top-level `u32` (id 1) and the string (id 8) for the self-check.
/// Fixed-size string buffer — no per-iteration heap allocation, like the C tool.
#[derive(Default)]
struct PerfOut {
    acc: u64,
    depth: i32,
    u32_top: u32,
    str_len: usize,
    str_buf: [u8; 32],
}

impl Visitor for PerfOut {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        self.acc = self.acc.wrapping_add(v ^ id as u64);
        if self.depth == 0 && id == 1 {
            self.u32_top = v as u32;
        }
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
    fn string(&mut self, id: Id, _total: usize, offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
        if id == 8 && offset < self.str_buf.len() {
            let end = (offset + chunk.len()).min(self.str_buf.len());
            self.str_buf[offset..end].copy_from_slice(&chunk[..end - offset]);
            self.str_len = end;
        }
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, chunk: &[u8]) {
        self.acc = self.acc.wrapping_add(chunk.len() as u64);
    }
    fn sequence_begin(&mut self, _id: Id) {
        self.depth += 1;
    }
    fn sequence_end(&mut self) {
        self.depth -= 1;
    }
}

fn perf_decode(buf: &[u8], out: &mut PerfOut) {
    let mut is = IStream::new();
    is.feed(buf, out).unwrap();
}

// ---------------------------------------------------------------------------
// large-array workload (identical to bench.rs / bench.c / bench.cpp): a
// standalone 1000-element u64 array. ARCHITECTURE.md §10 requires both
// benchmark tools to exercise this large array *and* the typical message.
// ---------------------------------------------------------------------------
const PERF_N: usize = 1000;

/// A spread of unsigned values exercising 1..10-byte varints (same generator as
/// bench.rs's `make_src`, so the encoded bytes match across the two tools).
fn perf_make_u64() -> Vec<u64> {
    (0..PERF_N as u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect()
}

fn perf_encode_u64(buf: &mut [u8], src: &[u64]) -> usize {
    let mut os = OStream::new(buf);
    os.write_array_unsigned(1, src).unwrap();
    os.bytes_used()
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------

/// Fixed iteration count from `SOFAB_PERF_ITERS`, replacing the adaptive ~1 s
/// loop with exactly N iterations of every workload.
///
/// This is what makes the instruction counts in the README reproducible: run the
/// tool under `valgrind --tool=callgrind` at two different N and difference the
/// totals — `(Ir(N2) - Ir(N1)) / (N2 - N1)` is the per-op instruction cost, with
/// process start-up, warm-up and reporting cancelling out. The time-derived
/// numbers the tool prints are meaningless in this mode (and under callgrind);
/// only the callgrind totals are.
/// How long one batch of operations runs before the clock is read again, in the
/// adaptive mode. See [`calibrate`].
const BATCH_SECS: f64 = 0.01;

/// Operations to run between clock reads: the smallest power of two whose run
/// spans [`BATCH_SECS`].
///
/// `clock_gettime(CLOCK_PROCESS_CPUTIME_ID)` is a real syscall — never
/// vDSO-accelerated — costing on the order of a microsecond, so reading it once
/// per iteration times the clock rather than the codec, and the cycle counter
/// bracketing the loop absorbs it too. On the 170-byte message that was ~10x on
/// `cycles/op`.
///
/// Only ever called in the adaptive mode: `SOFAB_PERF_ITERS` fixes the count
/// precisely so the Callgrind two-rep subtraction has a known, constant amount
/// of work to difference, and calibrating first would add an unknown amount to
/// it.
fn calibrate(mut body: impl FnMut()) -> u64 {
    let mut batch: u64 = 1;
    loop {
        let t0 = cpu_now();
        for _ in 0..batch {
            body();
        }
        if cpu_now() - t0 >= BATCH_SECS {
            return batch;
        }
        batch = batch.saturating_mul(2);
    }
}

fn fixed_iters() -> Option<u64> {
    std::env::var("SOFAB_PERF_ITERS").ok()?.parse().ok()
}

struct PerfResult {
    iters: u64,
    cycles_op: f64, // hardware cycles per operation
    ns_op: f64,     // CPU nanoseconds per operation
    mb_s: f64,      // throughput, MB/s (MB = 1e6 bytes)
}

fn perf_report(what: &str, r: &PerfResult, bytes: usize) {
    println!("\n--- perf: {what} ---");
    println!("  iterations    : {}", r.iters);
    println!("  message size  : {bytes} bytes");
    if cycles::HAVE {
        println!(
            "  cycles/op     : {:.1}  (hardware cycle counter)",
            r.cycles_op
        );
    } else {
        println!("  cycles/op     : (cycle counter unavailable on this arch)");
    }
    println!(
        "  CPU time/op   : {:.1} ns  (process CPU time, not wall-clock)",
        r.ns_op
    );
    println!(
        "  throughput    : {:.1} MB/s  (speedtest, MB = 1e6 bytes)",
        r.mb_s
    );
}

fn measure_encode(mut encode: impl FnMut() -> usize) -> (PerfResult, usize) {
    let mut msg = 0;
    for _ in 0..1000 {
        msg = encode(); // warmup
    }

    let fixed = fixed_iters();
    // One clock read per batch in both modes. Fixed mode is a single batch of
    // exactly N; adaptive mode sizes the batch so the read is negligible against
    // it, then runs batches to ~1 s.
    let batch = match fixed {
        Some(n) => n,
        None => calibrate(|| {
            black_box(encode());
        }),
    };
    let mut sink: usize = 0;
    let mut it: u64 = 0;
    let c0 = cycles::read();
    let t0 = cpu_now();
    let mut el;
    loop {
        for _ in 0..batch {
            sink = sink.wrapping_add(encode());
        }
        it += batch;
        el = cpu_now() - t0;
        if fixed.is_some() || el >= 1.0 {
            break;
        }
    }
    let c1 = cycles::read();
    black_box(sink);

    let r = PerfResult {
        iters: it,
        cycles_op: (c1 - c0) as f64 / it as f64,
        ns_op: el / it as f64 * 1e9,
        mb_s: msg as f64 * it as f64 / el / 1e6,
    };
    (r, msg)
}

fn measure_decode(buf: &[u8]) -> PerfResult {
    let mut out = PerfOut::default();
    for _ in 0..1000 {
        out = PerfOut::default();
        perf_decode(buf, &mut out); // warmup
    }
    black_box(out.acc);

    let fixed = fixed_iters();
    // See `measure_encode`: one clock read per batch in both modes.
    let batch = match fixed {
        Some(n) => n,
        None => calibrate(|| {
            let mut o = PerfOut::default();
            perf_decode(black_box(buf), &mut o);
            black_box(o.acc);
        }),
    };
    let mut sink: u64 = 0;
    let mut it: u64 = 0;
    let c0 = cycles::read();
    let t0 = cpu_now();
    let mut el;
    loop {
        for _ in 0..batch {
            let mut o = PerfOut::default();
            perf_decode(black_box(buf), &mut o);
            sink = sink.wrapping_add(o.acc);
        }
        it += batch;
        el = cpu_now() - t0;
        if fixed.is_some() || el >= 1.0 {
            break;
        }
    }
    let c1 = cycles::read();
    black_box(sink);

    PerfResult {
        iters: it,
        cycles_op: (c1 - c0) as f64 / it as f64,
        ns_op: el / it as f64 * 1e9,
        mb_s: buf.len() as f64 * it as f64 / el / 1e6,
    }
}

/// Which workloads to run, from `SOFAB_PERF_ONLY` (`encode` / `decode` /
/// `encode_u64` / `decode_u64`); unset runs all four, as it always has.
///
/// Companion to [`fixed_iters`]: an instruction-count profiler measures a whole
/// process, so isolating one workload's per-op cost means running only it.
fn selected(name: &str) -> bool {
    match std::env::var("SOFAB_PERF_ONLY") {
        Ok(only) => only == name,
        Err(_) => true,
    }
}

fn main() {
    let mut buffer = [0u8; 512];

    println!("=== SofaBuffers Rust per-op cost (cycles/op + throughput MB/s) ===");

    // The message is always encoded once, so the decode workloads have their
    // input even when the encode workload is not the one being measured.
    let mut msg_size = perf_encode(&mut buffer);
    if selected("encode") {
        // `black_box` on the destination buffer, mirroring the one on the decode
        // input: the encoded bytes are the same every iteration, so without it
        // the optimizer is free to hoist the whole workload out of the loop —
        // measured, it does exactly that, and the reported cost is then the
        // loop's, not the encoder's.
        let (enc, size) = measure_encode(|| perf_encode(black_box(&mut buffer)));
        msg_size = size;
        perf_report("serialize (stream API)", &enc, msg_size);
    }

    // Sanity check that the decode actually reproduced the data.
    let mut out = PerfOut::default();
    perf_decode(&buffer[..msg_size], &mut out);
    if out.u32_top != 0xDEAD_BEEF || &out.str_buf[..out.str_len] != PERF_STRING.as_bytes() {
        eprintln!("perf: decode self-check failed");
        std::process::exit(1);
    }

    if selected("decode") {
        let dec = measure_decode(&buffer[..msg_size]);
        perf_report("deserialize (stream API)", &dec, msg_size);
    }

    // Second reference workload (ARCHITECTURE.md §10): a standalone 1000-element
    // u64 array, measured with the exact same perf machinery as above.
    let src = perf_make_u64();
    let mut u64_buf = vec![0u8; PERF_N * 11 + 16];
    let mut u64_size = perf_encode_u64(&mut u64_buf, &src);

    if selected("encode_u64") {
        let (enc_u64, size) = measure_encode(|| perf_encode_u64(black_box(&mut u64_buf), &src));
        u64_size = size;
        perf_report("encode u64[1000] (stream API)", &enc_u64, u64_size);
    }

    if selected("decode_u64") {
        let dec_u64 = measure_decode(&u64_buf[..u64_size]);
        perf_report("decode u64[1000] (stream API)", &dec_u64, u64_size);
    }

    println!("\ncycles/op tracks code cost; MB/s is this machine's throughput.");
}
