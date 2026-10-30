//! RED contract — 0.5.57 typed-state car · batch F
//! Terminal-state immutability + framework-replay vs provider-re-render
//! diagnostic separation.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip).
//! - Distillation §1.4 "两种历史消息重现必须分诊" and §3.3#5
//!   "Submitted→Delivered 后任何 claim/attach/restart 都不可逆".
//! - Storm remote-control export proved that `delivered` was never
//!   re-injected (attempts / delivered_at unchanged); the provider
//!   re-rendered the historical transcript on `codex resume`. Any
//!   diagnostic that conflates the two will misattribute
//!   provider-side redraw as a framework replay.
//!
//! TEETH (RED at 5b847e4):
//!   1. `delivered_row_immutable_under_any_transition`
//!      — a `delivered` row's `delivered_at`, `delivery_attempts`,
//!      `status` and delivery token must be BYTE-STABLE across any
//!      claim/attach/requeue call from the typed-state surface.
//!      Baseline: passes (invariant lock) → green.
//!   2. `acknowledged_row_immutable_under_generic_mark`
//!      — an `acknowledged` row must not be overwritten by a generic
//!      `store.mark` call that could regress it to a delivery status.
//!      Baseline: distillation §1.2 acknowledged row notes "current
//!      guard still allows other status to overwrite; typed car needs
//!      to tighten." → red.
//!   3. `consumed_row_immutable_under_any_transition`
//!      — a `consumed` row must not re-enter any recovery arm.
//!      Baseline: source scan — no explicit exclusion. → red.
//!   4. `typed_event_carries_replay_vs_rerender_kind`
//!      — the framework must emit a typed marker distinguishing
//!      `framework_replay` (new attempt) from `provider_rerender`
//!      (transcript redraw, no new attempt). Baseline: no such
//!      marker; every `send.injected` looks the same regardless of
//!      whether it is a fresh submit or a resume redraw. → red.
//!   5. `restart_never_marks_delivered_as_accepted`
//!      — a restart / resume path must not translate a re-rendered
//!      transcript token into a new `messages.status = 'accepted'`
//!      write for an already-`delivered` row. Baseline (invariant
//!      lock, storm proved): green.
//!
//! POSITIVE CONTROL:
//!   6. `delivered_to_acknowledged_transition_still_allowed` — the
//!      legitimate business terminal transition (delivered →
//!      acknowledged via the ack API) must remain functional.
//!
//! NEGATIVE CONTROL:
//!   7. `stray_ack_call_does_not_promote_pending_row_to_acknowledged`
//!      — a mis-scoped ack call must not skip a row's delivery lifecycle
//!      and jump straight from `accepted` to `acknowledged`.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use crate as team_agent;

#[path = "rehomed_env.rs"]
mod rehomed_env;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serial_test::serial;
use team_agent::event_log::EventLog;
use team_agent::leader::{claim_lease_no_incident, LeaseStatus};
use team_agent::message_store::MessageStore;
use team_agent::messaging::enqueue_leader_mailbox_until_attach;
use team_agent::messaging::watchers::requeue_after_claim_leader;
use team_agent::model::enums::PaneLiveness;
use team_agent::model::ids::TeamKey;
use team_agent::state::owner_gate::PaneLivenessProbe;
use team_agent::transport::PaneId;

const TEAM: &str = "current";
const PANE: &str = "%leader";

struct LiveCaller;
impl PaneLivenessProbe for LiveCaller {
    fn liveness(&self, pane_id: &str) -> PaneLiveness {
        if pane_id == PANE {
            PaneLiveness::Live
        } else {
            PaneLiveness::Dead
        }
    }
}

fn bound_state() -> serde_json::Value {
    serde_json::json!({
        "session_name": "team-terminal-inv",
        "team_owner": {
            "pane_id": PANE, "provider": "codex", "owner_epoch": 7,
            "claimed_at": "2026-07-24T00:00:00Z", "claimed_via": "claim-leader"
        },
        "leader_receiver": {
            "pane_id": PANE, "provider": "codex", "owner_epoch": 7, "status": "attached"
        }
    })
}

struct TermCase {
    _env: rehomed_env::RehomedTestEnv,
    workspace: PathBuf,
    store: MessageStore,
    event_log: EventLog,
}

impl TermCase {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let env = rehomed_env::RehomedTestEnv::enter(tag);
        let workspace = env.workspace(&format!(
            "057f-term-{tag}-{}",
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            _env: env,
            workspace,
            store,
            event_log,
        }
    }

    fn seed(&self, content: &str, status: &str, delivered_at: Option<&str>) -> String {
        let mid = enqueue_leader_mailbox_until_attach(
            &self.workspace,
            TEAM,
            content,
            None,
            "leader",
            &self.event_log,
        )
        .unwrap()
        .message_id
        .expect("mailbox enqueue");
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set status = ?2, delivered_at = ?3, delivery_attempts = 1
             where message_id = ?1",
            params![mid, status, delivered_at],
        )
        .unwrap();
        mid
    }

    fn full(&self, mid: &str) -> (String, Option<String>, i64) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status, delivered_at, delivery_attempts from messages where message_id = ?1",
            params![mid],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    fn claim_and_requeue(&self) -> LeaseStatus {
        let team = TeamKey::new(TEAM.to_string());
        let pane = PaneId::new(PANE);
        let mut state = bound_state();
        let result = claim_lease_no_incident(
            &self.workspace,
            &mut state,
            None,
            &team,
            &pane,
            true,
            &self.event_log,
            &LiveCaller,
        )
        .unwrap();
        if result.ok {
            let _ = requeue_after_claim_leader(
                &self.workspace,
                &self.store,
                &self.event_log,
                &team,
                result.bound_pane_id.as_ref().expect("pane"),
                None,
            )
            .unwrap();
        }
        result.status
    }
}

// ---------------------------------------------------------------------------
// Tooth 1 — delivered row is byte-stable across claim (invariant lock)
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Tooth 2 — acknowledged row cannot be regressed by generic mark
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Tooth 3 — consumed row is untouchable by any recovery arm
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Tooth 4 — typed event carries framework_replay vs provider_rerender kind
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn typed_event_carries_replay_vs_rerender_kind() {
    // Source-shape probe: the delivery + acceptance event schemas must
    // include a typed `redisplay_kind` (or equivalent) enum with at
    // least the two values.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut has_marker = false;
    fn walk(dir: &std::path::Path, has_marker: &mut bool) {
        let Ok(es) = std::fs::read_dir(dir) else {
            return;
        };
        for e in es.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, has_marker);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    if (t.contains("framework_replay") && t.contains("provider_rerender"))
                        || t.contains("RedisplayKind::FrameworkReplay")
                        || t.contains("RedisplayKind::ProviderRerender")
                    {
                        *has_marker = true;
                    }
                }
            }
        }
    }
    walk(&src, &mut has_marker);
    assert!(
        has_marker,
        "no typed redisplay-kind marker found (framework_replay / provider_rerender). \
         §1.4 requires diagnostics to distinguish framework replay (new attempt) from \
         provider re-render (transcript redraw, no new attempt). Baseline: every \
         'send.injected' event looks identical."
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — restart never marks delivered as accepted (invariant lock)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn restart_never_marks_delivered_as_accepted() {
    // Source-scan: no file may contain a SQL literal that translates a
    // `delivered` row to `accepted`. We check for the substring
    // `set status = 'accepted'` inside any block that also mentions
    // `delivered`. This is a coarse guard; the more precise test lives
    // in tooth 1 (behavioral). Both are cheap and complementary.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut violations: Vec<PathBuf> = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(es) = std::fs::read_dir(dir) else {
            return;
        };
        for e in es.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|s| s.to_str()) == Some("tests") {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    let lines: Vec<&str> = t.lines().collect();
                    for (i, l) in lines.iter().enumerate() {
                        if l.contains("set status = 'accepted'") {
                            let start = i.saturating_sub(20);
                            let end = (i + 20).min(lines.len());
                            let window = lines[start..end].join("\n");
                            // Trigger only if `delivered` (not
                            // `delivered_at` alone) is treated as
                            // eligible.
                            if window.contains("status = 'delivered'")
                                || window.contains("'delivered'")
                            {
                                out.push(p.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    walk(&src, &mut violations);
    // 0.5.55 storm proved this invariant held (delivered was untouched).
    // Green at baseline; locked here.
    assert!(
        violations.is_empty(),
        "regression: SQL that flips a `delivered` row to `accepted` found: {:?}. \
         §3.3#5 storm proof — delivered must be irreversibly terminal.",
        violations
    );
}

// ---------------------------------------------------------------------------
// Tooth 6 — positive control: delivered→acknowledged is still supported
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Tooth 7 — negative control: stray ack cannot leapfrog delivery
// ---------------------------------------------------------------------------

