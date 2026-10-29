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
        Ok(format!(
            "framework-{}",
            digest_hex(digest(&serde_json::to_vec(&(
                &self.workspace,
                &self.team,
                &self.native_scope
            ))?))
        ))
    }
    fn validate(&self, store: &ContractStore) -> Result<(), Error> {
        if self.team.trim().is_empty()
            || self.team.chars().any(char::is_control)
            || store.scope() != &self.native_scope
            || resolve_cwd(&self.workspace.path).map_err(|_| Error::Fence)? != self.workspace
        {
            return Err(Error::Fence);
        }
        let selected = crate::state::selector::resolve_active_team_readonly(
            &self.workspace.path,
            Some(&self.team),
            crate::state::selector::SelectorMode::RuntimeOnly,
        )
        .map_err(|_| Error::Fence)?;
        if selected.team_key != self.team
            || resolve_cwd(&selected.run_workspace).map_err(|_| Error::Fence)? != self.workspace
        {
            return Err(Error::Fence);
        }
        Ok(())
    }
}

fn framework_result(result_id: &str, source: &str, envelope: &Value) -> Result<Value, Error> {
    if envelope.get("agent_id").and_then(Value::as_str) != Some(source)
        || envelope
            .get("task_id")
            .and_then(Value::as_str)
            .is_none_or(|task| task.trim().is_empty())
    {
        return Err(Error::Fence);
    }
    // K3's compact wire (schema 1 / completed) is not the root business envelope.
    // Reuse the shared normalizer rather than maintaining a second alias/schema
    // implementation or claiming that a persisted K3 result was forwarded.
    let normalized = crate::mcp_server::normalize::normalize_report_envelope(envelope);
    if normalized.presentation_error.is_some() {
        return Err(Error::Invalid("result presentation"));
    }
    let mut value = envelope.clone();
    value["presentation"] = serde_json::to_value(normalized.presentation)?;
    value["result_id"] = json!(result_id);
    Ok(value)
}

/// Reuses the common root send/result operations. No provider implementation is
/// selected here; the common per-recipient dispatcher remains authoritative.
fn forward_one(context: &FrameworkContext, intent: &ForwardIntent) -> (ForwardState, Value) {
    let run = || -> Result<Value, String> {
        let sender = intent.source.seat.as_str();
        match &intent.payload {
            ForwardPayload::Message { message, task, sender: claimed, recipient, content, mailbox } => {
                if claimed != sender { return Err("sender_binding_mismatch".into()); }
                let mut presentation = crate::messaging::presentation::PresentationRequest::default();
                if *mailbox {
                    presentation.sink = crate::messaging::presentation::PresentationSink::Casefile;
                    presentation.case_id = Some(message.as_str().into());
                }
                crate::station::send::execute(crate::station::send::Request {
                    workspace: &context.workspace.path, team: Some(&context.team),
                    to: recipient.as_str(), content, task_id: task.as_ref().map(|id| id.as_str()),
                    message_id: Some(message.as_str()), presentation: &presentation,
                    mailbox: *mailbox, envelope: None,
                }, crate::station::send::Authority::Seat { sender, team: &context.team })
            }
            ForwardPayload::Result { result_id, envelope, .. } => {
                let value = framework_result(result_id.as_str(), sender, envelope).map_err(|e| e.to_string())?;
                let normalized = crate::mcp_server::normalize::normalize_report_envelope(&value);
                let presentation = crate::messaging::presentation::PresentationRequest {
                    sink: normalized.presentation.sink, class: normalized.presentation.class,
                    case_id: normalized.presentation.case_id.clone(),
                };
                crate::station::send::execute(crate::station::send::Request {
                    workspace: &context.workspace.path, team: Some(&context.team), to: "leader",
                    content: value["summary"].as_str().unwrap_or(""),
                    task_id: value["task_id"].as_str(), message_id: None,
                    presentation: &presentation,
                    mailbox: presentation.sink != crate::messaging::presentation::PresentationSink::Leader,
                    envelope: Some(&value),
                }, crate::station::send::Authority::Seat { sender, team: &context.team })
            }
        }
    };
    match run() {
        Ok(value) => (ForwardState::Accepted, value),
        Err(error) => (ForwardState::Refused, json!({"code":"station_refused","error":error})),
    }
}

pub fn drain(
    context: &FrameworkContext,
    store: &mut ContractStore,
    limit: usize,
) -> Result<usize, Error> {
    context.validate(store)?;
    let route = context.route()?;
    let pending = store.pending_forwards(limit)?;
    let mut completed = 0;
    for next in pending {
        if next.source.scope != context.native_scope {
            return Err(Error::Fence);
        }
        let intent = match store.claim_forward(&next.id) {
            Ok(intent) => intent,
            Err(Error::Conflict) => continue,
            Err(error) => return Err(error),
        };
        let (state, receipt) = if intent.route != route {
            (
                ForwardState::Refused,
                json!({"code":"framework_route_changed"}),
            )
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

/// Explicit operator egress only: reuse committed native receipts, never run a
/// coordinator tick, provider admission, input delivery, or candidate migration.
/// The original runtime binding remains authoritative even when this helper was
/// built from a newer revision. Pending forwards retain their exact source IDs.
pub fn drain_scoped_pending_egress(
    workspace: &std::path::Path,
    team: &str,
    expected_scope: &str,
    apply: bool,
) -> Result<Value, String> {
    if !workspace.is_absolute() || team.is_empty() || team.trim() != team {
        return Err("egress requires an absolute workspace and exact explicit team".into());
    }
    ScopeId::new(expected_scope).map_err(|error| error.to_string())?;
    let selected = crate::state::selector::resolve_active_team_readonly(
        workspace, Some(team), crate::state::selector::SelectorMode::RuntimeOnly,
    ).map_err(|error| error.to_string())?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&selected.state, None)
        .map_err(|error| error.to_string())?;
    let original = super::backend::Backend::open_readonly(&selected.run_workspace, &selected.team_key)
        .map_err(|error| error.to_string())?;
    if original.binding.scope.as_str() != expected_scope || original.binding.team != team {
        return Err("egress scope/binding mismatch".into());
    }
    let binding = serde_json::to_value(&original.binding).map_err(|error| error.to_string())?;
    let before: Vec<_> = original.store.pending_forwards(32).map_err(|error| error.to_string())?
        .into_iter().map(|intent| intent.id).collect();
    drop(original);
    let processed = if apply {
        let mut backend = super::backend::Backend::open(&selected.run_workspace, &selected.team_key)
            .map_err(|error| error.to_string())?;
        if serde_json::to_value(&backend.binding).map_err(|error| error.to_string())? != binding {
            return Err("egress binding changed before claim".into());
        }
        drain(&backend.binding.framework(), &mut backend.store, 32).map_err(|error| error.to_string())?
    } else { 0 };
    let after = super::backend::Backend::open_readonly(&selected.run_workspace, &selected.team_key)
        .map_err(|error| error.to_string())?;
    if serde_json::to_value(&after.binding).map_err(|error| error.to_string())? != binding {
        return Err("egress binding changed during forwarding".into());
    }
    let pending: Vec<_> = after.store.pending_forwards(32).map_err(|error| error.to_string())?
        .into_iter().map(|intent| intent.id).collect();
    Ok(json!({"scope":expected_scope,"team":team,"apply":apply,"processed":processed,
        "pending_before":before,"pending_after":pending,"binding_unchanged":true,
        "native_input":"not_invoked","coordinator_lifecycle":"not_invoked",
        "leader_consumption":"requires actual natural receipt"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn egress_operator_refuses_implicit_workspace_team_and_invalid_scope_before_io() {
        for (workspace, team, scope) in [
            ("relative", "team", "scope"), ("/", "", "scope"),
            ("/", " team", "scope"), ("/", "team", ""), ("/", "team", "bad/scope"),
        ] {
            assert!(drain_scoped_pending_egress(std::path::Path::new(workspace), team, scope, false).is_err());
            assert!(drain_scoped_pending_egress(std::path::Path::new(workspace), team, scope, true).is_err());
        }
    }
    #[test]
    fn compact_native_result_becomes_the_existing_framework_business_schema() {
        let compact = json!({"schema_version":1,"agent_id":"worker","task_id":"msg-bound","status":"completed","summary":"5535"});
        let result = framework_result("result-fixed", "worker", &compact).unwrap();
        assert_eq!(result["schema_version"], "result_envelope_v1");
        assert_eq!(result["status"], "success");
        assert_eq!(result["task_id"], "msg-bound");
        assert_eq!(result["summary"], "5535");
        assert_eq!(result["result_id"], "result-fixed");
        for field in ["changes", "tests", "risks", "artifacts", "next_actions"] {
            assert!(result[field].is_array());
        }
        crate::messaging::helpers::validate_result_envelope(&result).unwrap();
        assert!(framework_result("result-fixed", "other", &compact).is_err());
    }
}
