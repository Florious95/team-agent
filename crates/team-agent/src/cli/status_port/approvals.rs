use super::*;

pub fn approvals(workspace: &Path, agent: Option<&str>, as_json: bool) -> Result<Value, CliError> {
    let _ = as_json;
    let state = read_runtime_state(workspace);
    approvals_scoped(workspace, &state, agent, as_json)
}

pub fn approvals_scoped(
    workspace: &Path,
    state: &Value,
    agent: Option<&str>,
    as_json: bool,
) -> Result<Value, CliError> {
    let _ = as_json;
    let session = state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut approvals = Vec::new();
    if let (Some(session), Some(agents)) = (session, state.get("agents").and_then(Value::as_object))
    {
        let run_ws = crate::model::paths::canonical_run_workspace(workspace)
            .unwrap_or_else(|_| workspace.to_path_buf());
        // 0.5.x Phase 1d Batch 3: use the factory-resolved backend
        // so conpty teams get their scrollback from the shim rather
        // than a fake tmux capture that always returns empty. Tmux
        // teams take the same code path as before (byte-equivalent).
        let resolved = crate::transport_factory::resolve_read_only_transport(
            &run_ws,
            Some(state),
            crate::transport_factory::TransportPurpose::Status,
        );
        let backend: Box<dyn crate::transport::Transport> = match resolved {
            Ok(r) => r.backend,
            Err(_) => {
                // Read-path fallback: refused factory means we don't
                // try to inspect approval prompts. Empty vec = no
                // waiting approvals, honest.
                return Ok(json!({
                    "ok": true,
                    "waiting": false,
                    "waiting_count": 0,
                    "approvals": [],
                }));
            }
        };
        for (agent_id, agent_state) in agents {
            if agent.is_some_and(|wanted| wanted != agent_id) {
                continue;
            }
            let window = agent_window(agent_id, agent_state);
            let target = crate::transport::Target::SessionWindow {
                session: crate::transport::SessionName::new(session.to_string()),
                window: crate::transport::WindowName::new(window.clone()),
            };
            let Ok(captured) = backend.capture(&target, crate::transport::CaptureRange::Tail(120))
            else {
                continue;
            };
            if let Some(prompt) = crate::provider::extract_approval_prompt(agent_id, &captured.text)
            {
                approvals.push(prompt.to_ordered_value());
            }
        }
    }
    let waiting_count = approvals.len();
    Ok(json!({
        "ok": true,
        "waiting": waiting_count > 0,
        "waiting_count": waiting_count,
        "approvals": approvals,
        "scan": {
            "mode": "tail",
            "lines": 120,
            "raw_output": false,
        },
    }))
}
