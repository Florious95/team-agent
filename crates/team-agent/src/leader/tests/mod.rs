#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::cell::Cell;

// ── helpers ────────────────────────────────────────────────────────────
/// derive 一个真 LeaderSessionUuid(骨架无公开裸构造器,仅 `derive`)。
fn uuid(fp: &str, ws: &str, user: &str, team: &str) -> LeaderSessionUuid {
    LeaderSessionUuid::derive(fp, ws, user, team).unwrap()
}

/// 注入式 turn-state 分类器:MUST-NOT-13 命门 —— 统计 classify 调用次数,
/// 据此断言 idle 面经此 trait 分类、绝不直连任何 provider client。
struct CountingClassifier {
    calls: Cell<usize>,
    result: TurnState,
}
impl CountingClassifier {
    fn new(result: TurnState) -> Self {
        Self {
            calls: Cell::new(0),
            result,
        }
    }
}
impl TurnStateClassifier for CountingClassifier {
    fn classify(
        &self,
        _provider: Provider,
        session_log_text: &str,
    ) -> Result<TurnClassification, LeaderError> {
        self.calls.set(self.calls.get() + 1);
        // 空 session-log 文本 → unknown(bug-085:None rollout_path 漏穿后读到空串)。
        let state = if session_log_text.is_empty() {
            TurnState::Unknown
        } else {
            self.result
        };
        Ok(TurnClassification {
            state,
            turn_id: None,
            annotations: vec![],
            reason: if state == TurnState::Unknown {
                Some("empty_session_log".into())
            } else {
                None
            },
        })
    }
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII env override: restores prior values on drop (incl. panic unwind), so a
/// RED assertion-panic cannot leak a dirty env into other tests.
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}
impl EnvGuard {
    fn apply(vars: &[(&str, Option<&str>)]) -> Self {
        let saved = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        Self { saved }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn p2_temp_ws(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("ta_rs_p2leader_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

// P1 — state-supplied leader_session_uuid → source 'derived' (NOT 'env'); Python's
// source is binary override-vs-derived (leader/__init__.py:206).
struct SeededLiveness {
    live_panes: std::collections::BTreeSet<String>,
}
impl crate::state::owner_gate::PaneLivenessProbe for SeededLiveness {
    fn liveness(&self, pane_id: &str) -> crate::model::enums::PaneLiveness {
        if self.live_panes.contains(pane_id) {
            crate::model::enums::PaneLiveness::Live
        } else {
            crate::model::enums::PaneLiveness::Dead
        }
    }
}

fn seeded_liveness(panes: &[&str]) -> SeededLiveness {
    SeededLiveness {
        live_panes: panes.iter().map(|p| (*p).to_string()).collect(),
    }
}

// RED — VACANT ACQUIRE: empty state + eligible caller → status=Claimed,
// reason=vacant_acquired, owner_epoch 0→1, owner+receiver bound to caller,
// claimed_via=claim-leader, BOTH state files written (dual-state).
// golden /tmp/probe_claimed.py: epoch 1, claimed_via "claim-leader",
// discovery "claim_leader", reason "vacant_acquired".
/// Event names from the workspace event log (events.jsonl), in write order.
fn event_names(ws: &std::path::Path) -> Vec<String> {
    let path = crate::model::paths::logs_dir(ws).join("events.jsonl");
    std::fs::read_to_string(&path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter_map(|e| e.get("event").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// D1 [BLOCK] — claim-path team_owner is EXACTLY golden's 7 keys (NO os_user). Golden new_owner
// (__init__.py:686-694) = pane_id,provider,machine_fingerprint,leader_session_uuid,owner_epoch,
// claimed_at,claimed_via; only Family-A leader_binding writes os_user, NEVER the claim path. Rust
// make_owner sets os_user:Some(..) + TeamOwner has no skip_serializing_if -> 8 keys. RED.

mod basics;
mod byte_findings;
mod identity;
mod idle;
mod lease_api;
mod lease_claim;
mod rediscover;
mod wake_start_owner;
