//! Run metrics for `--stats`: the records read from the main input, the records printed to
//! stdout, the input records skipped because they could not be parsed, and the elapsed time,
//! reported once as a JSON object on stderr when the program exits.
//!
//! Counting is off unless `--stats` is given; the counters are shared by all worker threads.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static REPORTED: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();
static INPUT: AtomicU64 = AtomicU64::new(0);
static OUTPUT: AtomicU64 = AtomicU64::new(0);
static REJECTED: AtomicU64 = AtomicU64::new(0);
/// Records of the main input that were header lines (`-H`, `-i jsonl`, ...), not data.
static INPUT_HEADERS: AtomicU64 = AtomicU64::new(0);
/// Records of stdout that are a header rather than data (`-o jsonl`, `-o parquet`, ...).
static OUTPUT_HEADERS: AtomicU64 = AtomicU64::new(0);

/// Turn on counting. Elapsed time is measured from `start`; with `header_output`, the first
/// record printed to stdout is the header of the output format and is not counted.
pub fn enable(start: Instant, header_output: bool) {
    let _ = START.set(start);
    OUTPUT_HEADERS.store(header_output as u64, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
}

#[inline]
fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A record was read from the main input.
#[inline]
pub(crate) fn input_record() {
    if enabled() {
        INPUT.fetch_add(1, Ordering::Relaxed);
    }
}

/// The record just read from the main input was a header line.
pub(crate) fn input_header() {
    if enabled() {
        INPUT_HEADERS.fetch_add(1, Ordering::Relaxed);
    }
}

/// `n` records were printed to stdout.
#[inline]
pub(crate) fn output_records(n: u64) {
    if enabled() {
        OUTPUT.fetch_add(n, Ordering::Relaxed);
    }
}

/// The records of a `printf` to stdout: the newlines it writes.
#[inline]
pub(crate) fn output_text(bs: &[u8]) {
    if enabled() {
        OUTPUT.fetch_add(
            memchr::memchr_iter(b'\n', bs).count() as u64,
            Ordering::Relaxed,
        );
    }
}

/// An input record was skipped because it could not be parsed (e.g. invalid JSON in
/// `-i jsonl`).
pub(crate) fn rejected_record() {
    if enabled() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Print the metrics to stderr, once, if `--stats` was given.
pub fn report(exit_code: i32) {
    if !enabled() || REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let elapsed = START.get().map_or(0.0, |s| s.elapsed().as_secs_f64());
    let input = INPUT
        .load(Ordering::Relaxed)
        .saturating_sub(INPUT_HEADERS.load(Ordering::Relaxed));
    let output = OUTPUT
        .load(Ordering::Relaxed)
        .saturating_sub(OUTPUT_HEADERS.load(Ordering::Relaxed));
    let per_sec = if elapsed > 0.0 {
        input as f64 / elapsed
    } else {
        0.0
    };
    eprintln_ignore!(
        "{{\"input_records\":{},\"output_records\":{},\"rejected_records\":{},\"elapsed_secs\":{:.3},\"records_per_sec\":{:.1},\"exit_code\":{}}}",
        input,
        output,
        REJECTED.load(Ordering::Relaxed),
        elapsed,
        per_sec,
        exit_code
    );
}
