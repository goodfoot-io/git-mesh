//! Opt-in performance logging for CLI operation groups.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init(cli_enabled: bool) {
    ENABLED.store(cli_enabled || env_enabled(), Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn env_enabled() -> bool {
    match std::env::var("GIT_MESH_PERF") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

pub struct Span {
    label: &'static str,
    start: Option<Instant>,
}

impl Span {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            start: enabled().then(Instant::now),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let elapsed = start.elapsed();
        eprintln!(
            "git-mesh perf: {} {:.3} ms",
            self.label,
            elapsed.as_secs_f64() * 1000.0
        );
    }
}

pub fn span(label: &'static str) -> Span {
    Span::new(label)
}

pub fn counter(label: &str, value: u64) {
    if !enabled() {
        return;
    }
    eprintln!("git-mesh perf: {label} {value}");
}

/// Emit a free-form annotation line in the `--perf` output.
///
/// Used to add context (e.g., tier-ordering legends) that does not map to a
/// numeric counter.  No-ops when perf output is disabled.
pub fn note(text: &str) {
    if !enabled() {
        return;
    }
    eprintln!("git-mesh perf: {text}");
}

// ── Subroutine-level counters ──────────────────────────────────────────────
//
// Process-global counters incremented from deep call sites that have no
// direct access to `ResolveSession`. The `git mesh stale` CLI invokes
// `reset()` at the top of `stale_meshes` and reads the values back inside
// the perf-emit block; output is only meaningful for a single resolver run
// per process invocation.

static GIX_OPEN_CALLS: AtomicU64 = AtomicU64::new(0);
/// Count of `attr_for` invocations. Each call probes gix's cached attribute
/// stack; the underlying `gix::index::File` is loaded from disk at most once
/// per `gix::Repository` instance, so this counter does not measure disk I/O.
static ATTR_FOR_CALLS: AtomicU64 = AtomicU64::new(0);
static IS_ANCESTOR_SUBPROCESS_CALLS: AtomicU64 = AtomicU64::new(0);
static IS_ANCESTOR_MEMO_HITS: AtomicU64 = AtomicU64::new(0);
static L1_HITS: AtomicU64 = AtomicU64::new(0);
static L1_MISSES: AtomicU64 = AtomicU64::new(0);
static L2_HITS: AtomicU64 = AtomicU64::new(0);
static L2_MISSES: AtomicU64 = AtomicU64::new(0);
static L2_READ_NS: AtomicU64 = AtomicU64::new(0);
static L2_WRITE_NS: AtomicU64 = AtomicU64::new(0);
static L2_BYTES_READ: AtomicU64 = AtomicU64::new(0);
static L2_BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);

pub fn record_gix_open() {
    if !enabled() {
        return;
    }
    GIX_OPEN_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_attr_for_call() {
    if !enabled() {
        return;
    }
    ATTR_FOR_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_is_ancestor_subprocess() {
    if !enabled() {
        return;
    }
    IS_ANCESTOR_SUBPROCESS_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_is_ancestor_memo_hit() {
    if !enabled() {
        return;
    }
    IS_ANCESTOR_MEMO_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_l1_hit() {
    if !enabled() {
        return;
    }
    L1_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_l1_miss() {
    if !enabled() {
        return;
    }
    L1_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_l2_hit() {
    if !enabled() {
        return;
    }
    L2_HITS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_l2_miss() {
    if !enabled() {
        return;
    }
    L2_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_l2_bytes_read(n: u64) {
    if !enabled() {
        return;
    }
    L2_BYTES_READ.fetch_add(n, Ordering::Relaxed);
}

pub fn record_l2_bytes_written(n: u64) {
    if !enabled() {
        return;
    }
    L2_BYTES_WRITTEN.fetch_add(n, Ordering::Relaxed);
}

pub fn time_l2_read<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    L2_READ_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    r
}

pub fn time_l2_write<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    L2_WRITE_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    r
}

pub fn gix_open_calls() -> u64 {
    GIX_OPEN_CALLS.load(Ordering::Relaxed)
}
pub fn attr_for_calls() -> u64 {
    ATTR_FOR_CALLS.load(Ordering::Relaxed)
}
pub fn is_ancestor_subprocess_calls() -> u64 {
    IS_ANCESTOR_SUBPROCESS_CALLS.load(Ordering::Relaxed)
}
pub fn is_ancestor_memo_hits() -> u64 {
    IS_ANCESTOR_MEMO_HITS.load(Ordering::Relaxed)
}
pub fn l1_hits() -> u64 {
    L1_HITS.load(Ordering::Relaxed)
}
pub fn l1_misses() -> u64 {
    L1_MISSES.load(Ordering::Relaxed)
}
pub fn l2_hits() -> u64 {
    L2_HITS.load(Ordering::Relaxed)
}
pub fn l2_misses() -> u64 {
    L2_MISSES.load(Ordering::Relaxed)
}
pub fn l2_read_us() -> u64 {
    L2_READ_NS.load(Ordering::Relaxed) / 1_000
}
pub fn l2_write_us() -> u64 {
    L2_WRITE_NS.load(Ordering::Relaxed) / 1_000
}
pub fn l2_bytes_read() -> u64 {
    L2_BYTES_READ.load(Ordering::Relaxed)
}
pub fn l2_bytes_written() -> u64 {
    L2_BYTES_WRITTEN.load(Ordering::Relaxed)
}
/// One row of per-anchor trace data emitted when `--perf-trace <path>` is set.
pub struct TraceRow {
    pub mesh: String,
    pub anchor_id: String,
    pub anchor_sha: String,
    pub path: String,
    pub wall_us: u128,
    pub fast_path: bool,
    pub status: &'static str,
}

/// Reset all subroutine-level counters. Called at the top of `stale_meshes`
/// so the emit block reports values from a single resolver run.
pub fn reset_subroutine_counters() {
    GIX_OPEN_CALLS.store(0, Ordering::Relaxed);
    ATTR_FOR_CALLS.store(0, Ordering::Relaxed);
    IS_ANCESTOR_SUBPROCESS_CALLS.store(0, Ordering::Relaxed);
    IS_ANCESTOR_MEMO_HITS.store(0, Ordering::Relaxed);
    L1_HITS.store(0, Ordering::Relaxed);
    L1_MISSES.store(0, Ordering::Relaxed);
    L2_HITS.store(0, Ordering::Relaxed);
    L2_MISSES.store(0, Ordering::Relaxed);
    L2_READ_NS.store(0, Ordering::Relaxed);
    L2_WRITE_NS.store(0, Ordering::Relaxed);
    L2_BYTES_READ.store(0, Ordering::Relaxed);
    L2_BYTES_WRITTEN.store(0, Ordering::Relaxed);
}
