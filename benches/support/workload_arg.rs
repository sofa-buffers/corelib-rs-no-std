//! Command-line workload selection shared by the bench binary and its tests.

/// Pick the Callgrind workload name out of a process argument list: the first
/// argument after the program name that is not a flag.
///
/// `benches/bench.rs` is a `harness = false` bench target, so cargo appends its
/// own `--bench` (and, for `cargo bench -- --nocapture`, libtest's flags) to
/// every run. Taking `argv[1]` verbatim would make `cargo bench --bench bench`
/// — the invocation the README documents — look like a request for a workload
/// named `--bench`. Skipping leading flags lets that invocation fall through to
/// the full throughput table, while `bench <workload>` from
/// `benches/run_callgrind.sh` still selects a single op.
///
/// A flag's *value* (`--test-threads 1`) is indistinguishable from a workload
/// name here; it lands on the unknown-workload path, which names it.
pub fn workload_arg<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().skip(1).find(|a| !a.starts_with('-'))
}
