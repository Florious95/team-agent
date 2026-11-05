//!
use crate::cli::{CliError, SendArgs};
use serde_json::{json, Value};

pub(super) fn logical_to_from_args(
    args: &SendArgs,
    host_leader_to: Option<&str>,
) -> Result<String, CliError> {
    if args.to_name.is_some()
        && (args.target.is_some() || args.targets.is_some() || args.to_leader.is_some())
    {
        return Err(CliError::Usage(
            "An agent name cannot be combined with --to-name/--pane/--to; choose one addressing mode".to_string(),
        ));
    }
    let supplied = [
        args.target.is_some(),
        args.targets.is_some(),
        args.to_name.is_some(),
        args.to_leader.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if supplied > 1 {
        return Err(CliError::Usage(
            "An agent name, --targets, --to-name and --to-leader are mutually exclusive; choose one addressing mode"
                .to_string(),
        ));
    }
    let logical_to = if args.to_leader.is_some() {
        host_leader_to.unwrap_or_default().to_string()
    } else if let Some(name) = args.to_name.as_deref() {
        name.to_string()
    } else if let Some(targets) = args.targets.as_deref() {
        targets.to_string()
    } else {
        args.target.clone().unwrap_or_default()
    };
    if args.target.is_none() && supplied > 0 && args.message.is_empty() {
        if args.to_name.is_some() {
            return Err(CliError::Usage(
                "Provide a task message; --to-name cannot send an empty message".to_string(),
            ));
        }
        return Err(CliError::Usage(
            "Provide a nonempty task message after the agent name, such as team-agent send worker 'Review the changes'".to_string(),
        ));
    }
    Ok(logical_to)
}

pub(super) fn adapt_positional_bare_error(
    args: &SendArgs,
    name: &str,
    error: &mut crate::cli::named_address::NamedAddressError,
) {
    if args.target.as_deref() != Some(name)
        || args.team.is_none()
        || name.contains('/')
        || name.contains(':')
        || name.contains(',')
    {
        return;
    }
    error.requested_name = Some(name.to_string());
    for candidate in &mut error.candidates {
        let agent_id = candidate
            .get("agent_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let (Some(object), Some(agent_id)) = (candidate.as_object_mut(), agent_id) {
            object.insert("name".to_string(), json!(agent_id));
        }
    }
    error.suggested_name = error
        .suggested_name
        .as_deref()
        .and_then(|suggested| suggested.rsplit('/').next())
        .map(str::to_string);
    if let Some(suggested) = error.suggested_name.as_deref() {
        error.action = format!("Did you mean {suggested}? Confirm the agent name; select the workspace and team with --workspace/--team.");
    }
}

#[cfg(test)]
mod a13_red_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use crate::cli::named_address::{NamedTargetKind, ResolvedNamedAddress};
    use crate::cli::SendArgs;
    use crate::messaging::TrustedSender;
    use crate::model::ids::AgentId;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fresh_workspace(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "team-agent-a13-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(workspace.join(".team/runtime")).expect("create fixture");
        workspace
    }

    fn seed_team(workspace: &Path, team: &str) {
        let session = format!("team-{team}");
        let state = json!({
            "active_team_key": team,
            "status": "alive",
            "session_name": session,
            "agents": {},
            "teams": {
                team: {
                    "status": "alive",
                    "session_name": session,
                    "agents": {},
                    "leader_receiver": {"status": "unbound"}
                }
            }
        });
        crate::state::persist::save_runtime_state(workspace, &state).expect("save fixture state");
        // M4: no SQLite message store fixture (MessageStore::open is retired).
    }

    fn send_args(workspace: &Path) -> SendArgs {
        SendArgs {
            target: None,
            message: vec!["payload".to_string()],
            targets: None,
            workspace: workspace.to_path_buf(),
            team: None,
            task: None,
            sender: TrustedSender::from_runtime_identity_with_source(
                AgentId::new("worker-1"),
                workspace.display(),
                "sender-team",
            ),
            no_ack: false,
            no_wait: true,
            watch_result: false,
            timeout: 0.0,
            confirm_human: false,
            json: true,
            message_id: None,
            presentation: crate::messaging::presentation::PresentationRequest::default(),
            pane: None,
            to_name: Some("other-team/leader".to_string()),
            to_leader: None,
        }
    }

    fn leader_address(sender_workspace: &Path, target_workspace: &Path) -> ResolvedNamedAddress {
        ResolvedNamedAddress {
            raw_name: format!("{}::other-team/leader", target_workspace.display()),
            target_kind: NamedTargetKind::Leader,
            sender_workspace: sender_workspace.to_path_buf(),
            target_workspace: target_workspace.to_path_buf(),
            team_key: Some("other-team".to_string()),
            agent_id: None,
            pane_id: String::new(),
            session_name: Some("team-other-team".to_string()),
            window_name: None,
            tmux_endpoint: None,
            transport_kind: Some("direct_tmux".to_string()),
            app_server: None,
            state_pane_id: None,
            state_pane_stale: false,
            agent_status: None,
            warning: None,
        }
    }



    /// Leader inbox records for `team` in `workspace` (M1: the inbox is the durable store).
    fn leader_records(workspace: &Path, team: &str) -> Vec<Value> {
        crate::inbox_store::read(workspace, "leader", 100, Some(team)).expect("read leader inbox")
            ["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// M1: the public send path is `cmd_send` -> `station::send`; SQLite rows are retired.
    #[test]
    fn a13_worker_cross_workspace_identity_is_preserved_in_row_and_render() {
        let sender_workspace = fresh_workspace("identity-sender");
        let target_workspace = fresh_workspace("identity-target");
        seed_team(&target_workspace, "other-team");
        let mut args = send_args(&sender_workspace);
        args.to_name = Some(format!("{}::other-team/leader", target_workspace.display()));
        args.message = vec!["identity payload".to_string()];

        crate::cli::send::cmd_send(&args).expect("cross-workspace identity send must persist");
        let records = leader_records(&target_workspace, "other-team");
        assert_eq!(records.len(), 1, "cross-workspace send must store once in the target: {records:?}");
        let sender = records[0]["sender"].as_str().unwrap_or_default().to_string();
        let expected = format!("{}::sender-team/worker-1", sender_workspace.display());
        assert_eq!(
            sender, expected,
            "durable record must retain the full sender identity"
        );
        let rendered = crate::station::send::render(&sender, "identity payload", "msg-a13", false);
        assert!(
            rendered.contains(&expected),
            "rendered pane must show full sender: {rendered}"
        );
        assert!(
            !rendered.contains("from leader"),
            "worker must not render as leader: {rendered}"
        );

        let _ = std::fs::remove_dir_all(sender_workspace);
        let _ = std::fs::remove_dir_all(target_workspace);
    }

    #[test]
    fn a13_worker_in_workspace_leader_send_remains_allowed() {
        let workspace = fresh_workspace("in-workspace");
        seed_team(&workspace, "other-team");
        let mut args = send_args(&workspace);
        args.to_name = Some(format!("{}::other-team/leader", workspace.display()));

        let result = crate::cli::send::cmd_send(&args)
            .expect("in-workspace leader send should preserve the existing path");
        let crate::cli::CmdOutput::Json(value) = result.output else {
            panic!("JSON send outcome expected")
        };
        assert_ne!(
            value["reason"],
            Value::String("target_out_of_scope".to_string()),
            "in-workspace send must not be classified as cross-workspace: {value}"
        );
        assert_eq!(
            leader_records(&workspace, "other-team").len(),
            1,
            "allowed send must persist once"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }
}
