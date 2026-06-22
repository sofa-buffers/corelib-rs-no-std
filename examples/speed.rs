//! Real wall-clock throughput (MB/s) for the Rust corelib — the timed
//! counterpart to the Callgrind benchmark in benches/perf.rs. Build with
//! opt-level 3 for a fair comparison with the C/C++/Go -O3 builds:
//!
//!   CARGO_PROFILE_RELEASE_OPT_LEVEL=3 cargo run --release --example speed
//!
//! Prints "<workload> <mbps>" lines.

use std::hint::black_box;
use std::time::Instant;

use sofab::{IStream, OStream, Visitor};

#[derive(Default)]
struct Counter {
    acc: u64,
}
impl Visitor for Counter {
    fn unsigned(&mut self, _id: sofab::Id, v: u64) {
        self.acc = self.acc.wrapping_add(v);
    }
    fn signed(&mut self, _id: sofab::Id, v: i64) {
        self.acc = self.acc.wrapping_add(v as u64);
    }
    fn fp32(&mut self, _id: sofab::Id, v: f32) {
        self.acc = self.acc.wrapping_add(v.to_bits() as u64);
    }
    fn string(&mut self, _id: sofab::Id, _t: usize, _o: usize, c: &[u8]) {
        self.acc = self.acc.wrapping_add(c.len() as u64);
    }
}

/// Run `f` for ~1s; return MB/s given the per-call message byte size.
fn bench(bytes: usize, mut f: impl FnMut()) -> f64 {
    f(); // warmup
    let t = Instant::now();
    let mut iters: u64 = 0;
    while t.elapsed().as_secs_f64() < 1.0 {
        f();
        iters += 1;
    }
    let secs = t.elapsed().as_secs_f64();
    (bytes as f64) * (iters as f64) / secs / 1e6
}

fn main() {
    let src: Vec<u64> = (0..1000u64)
        .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .collect();

    // pre-encode messages for the decode loops + to learn their byte sizes.
    let mut tmp = vec![0u8; 16 * 1024];
    let enc_u64: Vec<u8> = {
        let mut os = OStream::new(&mut tmp);
        os.write_array_unsigned(1, &src).unwrap();
        let n = os.bytes_used();
        tmp[..n].to_vec()
    };
    let enc_typ: Vec<u8> = {
        let mut buf = [0u8; 256];
        let mut os = OStream::new(&mut buf);
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
        let n = os.bytes_used();
        buf[..n].to_vec()
    };

    let ba = enc_u64.len();
    let bt = enc_typ.len();

    // encode: u64 array
    let mut buf = [0u8; 16 * 1024];
    println!(
        "encode_u64_array {:.2}",
        bench(ba, || {
            let mut os = OStream::new(&mut buf);
            os.write_array_unsigned(1, black_box(&src)).unwrap();
            black_box(os.bytes_used());
        })
    );

    // encode: typical message
    let mut buf2 = [0u8; 256];
    println!(
        "encode_typical {:.2}",
        bench(bt, || {
            let mut os = OStream::new(&mut buf2);
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
            black_box(os.bytes_used());
        })
    );

    // decode: u64 array
    println!(
        "decode_u64_array {:.2}",
        bench(ba, || {
            let mut c = Counter::default();
            let mut is = IStream::new();
            is.feed(black_box(&enc_u64), &mut c).unwrap();
            black_box(c.acc);
        })
    );

    // decode: typical message
    println!(
        "decode_typical {:.2}",
        bench(bt, || {
            let mut c = Counter::default();
            let mut is = IStream::new();
            is.feed(black_box(&enc_typ), &mut c).unwrap();
            black_box(c.acc);
        })
    );
}
