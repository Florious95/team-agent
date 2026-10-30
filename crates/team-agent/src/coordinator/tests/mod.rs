#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;

// ─────────────────────────────────────────────────────────────────────────
// helpers — concrete golden fixtures (Python v0.2.11 @ 439bef8)
// ─────────────────────────────────────────────────────────────────────────

/// in-memory `MarkerStore` —— durable marker 落盘探针。
struct MapMarkerStore {
    markers: std::collections::BTreeMap<String, Value>,
    fail: bool,
}
impl MapMarkerStore {
    fn ok() -> Self {
        Self {
            markers: Default::default(),
            fail: false,
        }
    }
    fn failing() -> Self {
        Self {
            markers: Default::default(),
            fail: true,
        }
    }
}
impl MarkerStore for MapMarkerStore {
    fn set_marker(&mut self, name: &str, value: Value) -> bool {
        if self.fail {
            return false;
        }
        self.markers.insert(name.to_string(), value);
        true
    }
}

mod s3_bounded;
mod abnormal;
mod tick_core;
