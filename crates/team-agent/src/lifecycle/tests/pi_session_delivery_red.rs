use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::lifecycle::launch::pi_mcp::pi_seat_paths;
use crate::messaging::delivery::{
    paste_to_submit_floor_for_recipient, recipient_allows_explicit_queue_flush, recipient_is_busy,
    recipient_requires_single_enter,
};
use crate::model::enums::{Provider, ProviderEffort};
use crate::provider::adapters::pi::{build_pi_command_argv, PiCommandRequest, PiSessionSelector};
use crate::provider::session_scan::{scan_session_candidates_once, CaptureSessionContext};
use crate::provider::{get_adapter, AuthMode, ProviderError, SessionId};

static NEXT_ROOT: AtomicU32 = AtomicU32::new(0);

fn temp_root(label: &str) -> PathBuf {
    let seq = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "team-agent-pi-{label}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn write_header(path: &Path, id: &str, cwd: &Path, timestamp: &str) {
    std::fs::create_dir_all(path.parent().expect("session parent")).expect("create session parent");
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": id,
        "cwd": cwd,
        "timestamp": timestamp,
    });
    std::fs::write(
        path,
        format!("{header}\nsecret conversation body must not be read\n"),
    )
    .expect("write session fixture");
}

fn scan_context(cwd: &Path, root: &Path, id: &str, generation: &str) -> CaptureSessionContext {
    CaptureSessionContext {
        agent_id: "worker-a".to_string(),
        spawn_cwd: cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(generation.to_string()),
        expected_session_id: Some(SessionId::new(id)),
        provider_projects_root: Some(root.to_path_buf()),
    }
}

fn resume_argv(path: &Path) -> Result<Vec<String>, ProviderError> {
    build_pi_command_argv(PiCommandRequest {
        executable: Path::new("/verified/pi"),
        extension: Path::new("/workspace/.team/runtime/pi/team-a/worker-a/team-mcp.ts"),
        model: "team-agent/qwen3.8-27b",
        effort: ProviderEffort::Medium,
        system_prompt: "worker contract",
        tool_categories: &["mcp_team"],
        session_dir: Path::new("/workspace/.team/runtime/pi/team-a/worker-a/sessions"),
        session: PiSessionSelector::Resume { path },
        agent_id: "worker-a",
    })
}

#[test]
fn pi_session_capture_requires_header_id_cwd_generation_and_unique_claim() {
    let root = temp_root("session-capture");
    let cwd = root.join("workspace");
    let sessions = root.join("seat/sessions");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let id = "652756ea-aaee-4437-a30f-40c5606826d5";
    let generation = "2026-08-29T10:00:00Z";
    let session_timestamp = "2026-08-29T10:00:01Z";
    let expected_path = sessions.join("2026/08/29/session.jsonl");
    write_header(&expected_path, id, &cwd, session_timestamp);

    let context = scan_context(&cwd, &sessions, id, generation);
    let candidates = scan_session_candidates_once(Provider::Pi, &context).expect("Pi scan");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0]
            .captured
            .session_id
            .as_ref()
            .map(SessionId::as_str),
        Some(id)
    );
    assert_eq!(
        candidates[0]
            .captured
            .rollout_path
            .as_ref()
            .map(|path| path.as_path()),
        Some(expected_path.as_path())
    );

    for (label, header_id, header_cwd, header_timestamp) in [
        ("wrong-id", "wrong-id", cwd.as_path(), session_timestamp),
        ("wrong-cwd", id, root.as_path(), session_timestamp),
        (
            "wrong-generation",
            id,
            cwd.as_path(),
            "2026-08-29T09:00:00+00:00",
        ),
    ] {
        let case_root = root.join(label);
        let case_path = case_root.join(format!("{id}.jsonl"));
        write_header(&case_path, header_id, header_cwd, header_timestamp);
        let case_context = scan_context(&cwd, &case_root, id, generation);
        assert!(
            scan_session_candidates_once(Provider::Pi, &case_context)
                .expect("negative scan")
                .is_empty(),
            "filename or mtime must not override a mismatched header: {label}"
        );
    }

    let second = sessions.join("duplicate/session.jsonl");
    write_header(&second, id, &cwd, session_timestamp);
    assert_eq!(
        scan_session_candidates_once(Provider::Pi, &context)
            .expect("ambiguous scan")
            .len(),
        2,
        "multiple exact candidates must remain ambiguous for the shared allocator"
    );

    std::fs::remove_dir_all(root).expect("remove session fixture");
}

#[test]
fn pi_restart_uses_exact_path_and_refuses_missing_backing() {
    let root = temp_root("resume");
    let cwd = root.join("workspace");
    let sessions = root.join("seat/sessions");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let id = "cc09d133-878f-4241-920f-f7ebfefc2a9e";
    let generation = "2026-08-29T10:00:00Z";
    let exact = sessions.join("nested/exact.jsonl");
    write_header(&exact, id, &cwd, "2026-08-29T10:00:01Z");

    let candidates =
        scan_session_candidates_once(Provider::Pi, &scan_context(&cwd, &sessions, id, generation))
            .expect("resume preflight scan");
    assert_eq!(
        candidates.len(),
        1,
        "exact backing must revalidate before restart"
    );

    let argv = resume_argv(&exact).expect("resume exact path");
    assert!(argv
        .windows(2)
        .any(|pair| { pair[0] == "--session" && pair[1] == exact.to_string_lossy().as_ref() }));
    assert!(!argv.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--session-id" | "-c" | "--continue" | "-r" | "--resume"
        )
    }));

    std::fs::remove_file(&exact).expect("remove backing tooth");
    let missing = resume_argv(&exact);
    assert!(
        matches!(missing, Err(ProviderError::ResumeUnavailable(_))),
        "missing exact backing must refuse rather than create a new session: {missing:?}"
    );
    std::fs::remove_dir_all(root).expect("remove resume fixture");
}

#[test]
fn pi_delivery_submits_one_enter_and_never_flushes_or_retries() {
    let idle = serde_json::json!({
        "agents": {
            "worker-a": {"provider": "pi", "status": "running"}
        }
    });
    assert!(recipient_requires_single_enter(&idle, "worker-a"));
    assert!(!recipient_allows_explicit_queue_flush(&idle, "worker-a"));
    assert_eq!(
        paste_to_submit_floor_for_recipient(&idle, "worker-a"),
        Duration::ZERO
    );
    assert!(!recipient_is_busy(&idle, "worker-a"));

    let busy = serde_json::json!({
        "agents": {
            "worker-a": {"provider": "pi", "status": "busy"}
        }
    });
    assert!(
        recipient_is_busy(&busy, "worker-a"),
        "known busy must take the existing send.deferred_busy path before paste"
    );
    assert!(recipient_requires_single_enter(&busy, "worker-a"));
    assert!(!recipient_allows_explicit_queue_flush(&busy, "worker-a"));
}

#[test]
fn pi_clone_is_fresh_and_fork_is_not_clone() {
    let workspace = Path::new("/workspace");
    let source_paths = pi_seat_paths(workspace, "team-a", "source");
    let clone_paths = pi_seat_paths(workspace, "team-a", "clone");
    assert_ne!(source_paths.runtime_root, clone_paths.runtime_root);
    assert_ne!(source_paths.wrapper, clone_paths.wrapper);
    assert_ne!(source_paths.sessions, clone_paths.sessions);

    let source_session = SessionId::new("652756ea-aaee-4437-a30f-40c5606826d5");
    let fork = get_adapter(Provider::Pi).fork(Some(&source_session), AuthMode::Subscription, None);
    assert!(matches!(fork, Err(ProviderError::CapabilityUnsupported(_))));
}
