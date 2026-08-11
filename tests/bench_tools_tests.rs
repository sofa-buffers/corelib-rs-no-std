//! Regression tests for the bench tooling's command line (CORELIB_PLAN §10).
//!
//! `benches/bench.rs` is a `harness = false` bench target, so `cargo bench
//! --bench bench` — the invocation the README documents — runs the binary with
//! cargo's own `--bench` flag appended. The binary must ignore cargo's flags and
//! print the throughput table, while still accepting the bare workload name that
//! `benches/run_callgrind.sh` passes.

#[path = "../benches/support/workload_arg.rs"]
mod workload_arg;

use workload_arg::workload_arg;

fn args(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_string()).collect()
}

/// `cargo bench --bench bench` execs the binary as `bench --bench`: no workload
/// was requested, so the full table must run.
#[test]
fn cargo_bench_flag_is_not_a_workload() {
    assert_eq!(workload_arg(args(&["bench", "--bench"])), None);
}

/// Plain `cargo bench` (all bench targets) appends the same flag.
#[test]
fn libtest_flags_are_not_workloads() {
    assert_eq!(
        workload_arg(args(&["bench", "--bench", "--nocapture"])),
        None
    );
    assert_eq!(
        workload_arg(args(&["bench", "--test-threads", "1"])),
        Some("1".to_string()),
        "a flag's own value is indistinguishable from a workload; \
         the unknown-workload path reports it"
    );
}

/// Every workload `benches/run_callgrind.sh` asks for, in the order its table
/// prints them (BENCH_SPEC's row order). Kept here so the two lists cannot drift:
/// a name the script passes and the binary does not know exits 2, and the script
/// then prints a dash where an instruction count belongs.
const WORKLOADS: [&str; 10] = [
    "encode_u64_array",
    "encode_typical",
    "encode_blob_oneshot",
    "encode_blob_streaming",
    "encode_composite",
    "decode_u64_array",
    "decode_typical",
    "decode_blob",
    "decode_composite",
    "decode_composite_skip",
];

/// `run_callgrind.sh` invokes the binary directly as `bench <workload>`.
#[test]
fn bare_workload_name_is_selected() {
    for w in WORKLOADS {
        assert_eq!(workload_arg(args(&["bench", w])), Some(w.to_string()));
    }
}

/// The script and the binary agree on the workload names.
///
/// `run_callgrind.sh` is a shell script, so nothing but this test connects its
/// `WORKLOADS` array to the `match` in `benches/bench.rs` — and a mismatch is
/// silent in the place it matters least (a dash in a table) and loud nowhere.
#[test]
fn callgrind_script_and_binary_agree_on_the_workloads() {
    let script = include_str!("../benches/run_callgrind.sh");
    let bench = include_str!("../benches/bench.rs");
    for w in WORKLOADS {
        assert!(
            script.contains(&format!("\n    {w}\n")),
            "benches/run_callgrind.sh does not run the {w} workload"
        );
        assert!(
            bench.contains(&format!("\"{w}\" =>")),
            "benches/bench.rs does not implement the {w} workload"
        );
        assert!(
            bench.contains(&format!("pub fn run_{w}(")),
            "benches/bench.rs has no run_{w} symbol for --toggle-collect"
        );
    }
}

/// A workload that follows cargo's flags is still found.
#[test]
fn workload_after_flags_is_selected() {
    assert_eq!(
        workload_arg(args(&["bench", "--bench", "encode_typical"])),
        Some("encode_typical".to_string())
    );
}

/// No arguments at all (the table run).
#[test]
fn no_arguments_means_no_workload() {
    assert_eq!(workload_arg(args(&["bench"])), None);
}
