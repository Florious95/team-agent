//! New-runtime egress into the existing shared framework business APIs. A
//! forwarding intent is not a legacy physical queue, nor proof of presentation.
use serde_json::{json, Value};
use team_agent_contract::contract::{session::CwdIdentity, types::ScopeId};
use team_agent_contract::host::{digest, digest_hex, process::resolve_cwd};
use team_agent_contract::orchestration::{forward::*, store::ContractStore, Error};

use crate::messaging::{self, MessageTarget, SendOptions, SendOrigin, TrustedSender};
use crate::model::ids::{AgentId, TaskId, TeamKey};

/// Created from the selected framework Team, never from MCP tool arguments.
pub struct FrameworkContext {
    pub workspace: CwdIdentity,
    pub team: String,
    pub native_scope: ScopeId,
}
impl FrameworkContext {
    pub fn route(&self) -> Result<String, Error> {
        Ok(format!("framework-{}", digest_hex(digest(&serde_json::to_vec(&(&self.workspace, &self.team, &self.native_scope))?))))
    }
    fn validate(&self, store: &ContractStore) -> Result<(), Error> {
        if self.team.trim().is_empty() || self.team.chars().any(char::is_control)
            || store.scope() != &self.native_scope
            || resolve_cwd(&self.workspace.path).map_err(|_| Error::Fence)? != self.workspace {
            return Err(Error::Fence);
        }
        let selected = crate::state::selector::resolve_active_team_readonly(
            &self.workspace.path, Some(&self.team), crate::state::selector::SelectorMode::RuntimeOnly,
        ).map_err(|_| Error::Fence)?;
        if selected.team_key != self.team || resolve_cwd(&selected.run_workspace).map_err(|_| Error::Fence)? != self.workspace {
            return Err(Error::Fence);
        }
        Ok(())
    }
}

fn framework_result(result_id: &str, source: &str, envelope: &Value) -> Result<Value, Error> {
    if envelope.get("agent_id").and_then(Value::as_str) != Some(source)
        || envelope.get("task_id").and_then(Value::as_str).is_none_or(|task| task.trim().is_empty()) {
        return Err(Error::Fence);
    }
    // K3's compact wire (schema 1 / completed) is not the root business envelope.
    // Reuse the shared normalizer rather than maintaining a second alias/schema
    // implementation or claiming that a persisted K3 result was forwarded.
    let normalized = crate::mcp_server::normalize::normalize_report_envelope(envelope);
    if normalized.presentation_error.is_some() { return Err(Error::Invalid("result presentation")); }
    let mut value = serde_json::to_value(normalized)?;
    value["result_id"] = json!(result_id);
    Ok(value)
}

/// Reuses the common root send/result operations. No provider implementation is
/// selected here; the common per-recipient dispatcher remains authoritative.
fn forward_one(context: &FrameworkContext, intent: &ForwardIntent) -> (ForwardState, Value) {
    match &intent.payload {
        ForwardPayload::Message { message, task, sender, recipient, content, mailbox } => {
            if sender != intent.source.seat.as_str() {
                return (ForwardState::Refused, json!({"code":"sender_binding_mismatch"}));
            }
            let mut options = SendOptions {
                origin: SendOrigin::Mcp,
                task_id: task.as_ref().map(|task| TaskId::new(task.clone())),
                route_task_id: false,
                sender: TrustedSender::from_runtime_identity(AgentId::new(sender)),
                team: Some(TeamKey::new(&context.team)),
                message_id: Some(message.as_str().into()),
                wait_visible: false, block_until_delivered: false,
                timeout: 2.0, ..Default::default()
            };
            if *mailbox { options.presentation.sink = messaging::presentation::PresentationSink::Casefile; }
            match messaging::send_message(&context.workspace.path, &MessageTarget::Single(recipient.clone()), content, &options) {
                Ok(outcome) => {
                    let accepted = outcome.message_id.as_deref() == Some(message.as_str())
                        && matches!(outcome.status, messaging::DeliveryStatus::Queued
                            | messaging::DeliveryStatus::StoredOnly | messaging::DeliveryStatus::Delivered
                            | messaging::DeliveryStatus::AlreadyDelivered | messaging::DeliveryStatus::RetryScheduled
                            | messaging::DeliveryStatus::FallbackLog);
                    let state = if accepted { ForwardState::Accepted }
                        else if outcome.message_id.is_none() && matches!(outcome.status, messaging::DeliveryStatus::Refused) { ForwardState::Refused }
                        else { ForwardState::Unknown };
                    (state, json!({"ok":outcome.ok,"message_id":outcome.message_id,
                        "status":outcome.status,"message_status":outcome.message_status.0,
                        "reason":outcome.reason,"verification":outcome.verification}))
                }
                // Error alone is not proof that no destination row was written.
                Err(_) => (ForwardState::Unknown, json!({"code":"framework_send_outcome_unknown"})),
            }
        }
        ForwardPayload::Result { result_id, envelope } => {
            let envelope = match framework_result(result_id, intent.source.seat.as_str(), envelope) {
                Ok(envelope) => envelope,
                Err(_) => return (ForwardState::Refused, json!({"code":"invalid_result_envelope"})),
            };
            match messaging::results::report_result_for_owner_team(&context.workspace.path, &envelope, Some(&context.team)) {
                Ok(receipt) => (ForwardState::Accepted, receipt),
                Err(_) => (ForwardState::Unknown, json!({"code":"framework_result_outcome_unknown"})),
            }
        }
    }
}

/// The root coordinator owns this consumer. Claim commits before the delegate
/// is called, so no K3 transaction is held across root framework locks/writes.
/// InFlight/Unknown items require explicit exact-ID reconciliation, not replay.
pub fn drain(context: &FrameworkContext, store: &mut ContractStore, limit: usize) -> Result<usize, Error> {
    context.validate(store)?;
    let route = context.route()?;
    let pending = store.pending_forwards(limit)?;
    let mut completed = 0;
    for next in pending {
        if next.source.scope != context.native_scope { return Err(Error::Fence); }
        let intent = match store.claim_forward(&next.id) {
            Ok(intent) => intent,
            Err(Error::Conflict) => continue,
            Err(error) => return Err(error),
        };
        let (state, receipt) = if intent.route != route {
            (ForwardState::Refused, json!({"code":"framework_route_changed"}))
        } else {
            // Recheck the selected directory before every external operation.
            context.validate(store)?;
            forward_one(context, &intent)
        };
        store.finish_forward(&intent.id, state, receipt)?;
        completed += 1;
    }
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_native_result_becomes_the_existing_framework_business_schema() {
        let compact = json!({"schema_version":1,"agent_id":"worker","task_id":"msg-bound","status":"completed","summary":"5535"});
        let result = framework_result("result-fixed", "worker", &compact).unwrap();
        assert_eq!(result["schema_version"], "result_envelope_v1");
        assert_eq!(result["status"], "success");
        assert_eq!(result["task_id"], "msg-bound");
        assert_eq!(result["summary"], "5535");
        assert_eq!(result["result_id"], "result-fixed");
        for field in ["changes", "tests", "risks", "artifacts", "next_actions"] { assert!(result[field].is_array()); }
        crate::messaging::helpers::validate_result_envelope(&result).unwrap();
        assert!(framework_result("result-fixed", "other", &compact).is_err());
    }
}
