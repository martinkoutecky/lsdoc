//! Complexity instrumentation — a debug-only thread-local counter of "scan work": all work done
//! by parser-owned scans and indexing structures. That includes byte scans plus index builds,
//! cursor advances, cache lookups, search probes, and tree descents. An uncharged loop in a
//! parser-owned structure is a complexity bug.
//!
//! # Why this exists
//! The byte-exact parity gate (`harness/`) verifies WHAT the parser produces, not HOW MUCH work
//! it does — it is structurally blind to O(n²) re-scans (the 2026-07 audit found four O(n²)
//! families while the parity gate read 1321/1321). Timing gates are noisy and only test the
//! shapes you thought of. This counter is a **deterministic** complexity signal: the complexity
//! gate (`tests/complexity.rs`) parses adversarial families at n / 2n / 4n and asserts the count
//! grows ~linearly. A re-scan makes it grow ~quadratically, failing the gate — regardless of
//! machine noise or which exact input triggers it.
//!
//! # Invariant
//! `scan_work` summed over a parse must be **O(input length)**. Every increment marks parser-owned
//! work that must be amortized by a single-pass design: byte walks, index construction, cursor
//! advances, cache lookup/comparison, search probes, and tree visits.
//!
//! # Zero cost in release
//! The counter and every `scan_work` body are `#[cfg(debug_assertions)]`, so release builds (and
//! Tine, which links the release lib) compile them to nothing. The gate runs in a debug build.

#[cfg(debug_assertions)]
thread_local! {
    static SCAN_WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Charge `n` units of parser-owned scan/index work to the debug counter. No-op (and fully
/// compiled out) in release.
#[inline(always)]
pub(crate) fn scan_work(_n: usize) {
    #[cfg(debug_assertions)]
    SCAN_WORK.with(|c| c.set(c.get().wrapping_add(_n as u64)));
}

/// Read and zero the scan-work counter. Debug-only; the complexity gate calls this around a parse.
#[cfg(debug_assertions)]
pub fn __scan_work_take() -> u64 {
    SCAN_WORK.with(|c| c.replace(0))
}
