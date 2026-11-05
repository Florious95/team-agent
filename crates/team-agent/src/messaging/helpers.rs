//! Identifiers for result envelopes and runs.

use std::sync::atomic::{AtomicU64, Ordering};

static RESULT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_result_id() -> String {
    let n = RESULT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("res_{:012x}", (nanos ^ u128::from(n)) & 0xFFFF_FFFF_FFFF)
}

pub(crate) fn next_run_id() -> String {
    let id = next_result_id();
    id.chars().filter(|c| *c != '_').take(12).collect()
}
