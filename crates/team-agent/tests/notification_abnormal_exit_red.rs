//! ---
//! purpose: 钉死 worker abnormal-exit 只有 fresh/latest explicit provider error 才经 N32 leader funnel 排队一次；stale 与 dead-only 必须静默。
//! contract:
//!   provides:
//!     - name: T6-fresh-latest-error-leader-funnel
//!       what: coordinator tick 以 fresh explicit error 通知 leader；重复 fingerprint 去重，stale/dead-only 不通知
//!     - name: T6-claude-api-error-classification
//!       what: Claude API-error record 保持 explicit error fact 的单一分类来源
//!   depends:
//!     - team_agent::coordinator::Coordinator::tick
//!     - team_agent::messaging::send_to_leader_receiver
//!     - team_agent::provider::latest_explicit_error_fact
//!     - tests::support::hermetic::HermeticTestEnv
//! boundary:
//!   - 不改 abnormal-notification 产品路径，不恢复 global error_only scan
//!   - MessageStore/workspace 必须在 HermeticTestEnv 后创建；不接触 ambient HOME/TMUX/registry
//! maturity: wired
//! ---
//!
//! #236 abnormal-exit notification contracts.
//!
//! User-facing invariant: a worker abnormal-exit notification is deterministic and zero-false-positive:
//! the latest transcript/rollout fact must be an explicit provider error that is fresh for the
//! current worker cohort. Process liveness is an audit field; dead-without-error remains silent.
//! The notification goes through the N32 leader funnel.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;


use hermetic_guard::HermeticTestEnv;
use rusqlite::Connection;
use serial_test::serial;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{send_to_leader_receiver, DeliveryStatus};
use team_agent::model::ids::TaskId;
use team_agent::provider::{latest_explicit_error_fact, FactKind, Provider};


#[test]
fn notification_abnormal_exit_claude_api_error_shape_is_classified() {
    let record = serde_json::json!({
        "type": "assistant",
        "uuid": "assistant-404",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "API Error"}]},
        "error": "model_not_found",
        "isApiErrorMessage": true,
        "apiErrorStatus": 404,
        "requestId": "req-404"
    });
    let fact = latest_explicit_error_fact(Provider::ClaudeCode, &format!("{record}\n"))
        .expect("assistant API-error record is an explicit Claude fault");
    assert_eq!(fact.kind, FactKind::Error);
    assert_eq!(
        fact.turn_id.as_ref().map(|id| id.as_str()),
        Some("assistant-404")
    );
    assert_eq!(fact.api_error_status, Some(404));
    assert_eq!(fact.error.as_deref(), Some("model_not_found"));
    assert_eq!(fact.request_id.as_deref(), Some("req-404"));
    assert_eq!(fact.assistant_uuid.as_deref(), Some("assistant-404"));
}
