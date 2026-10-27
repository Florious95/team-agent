use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::model::enums::Provider;
use crate::provider::session_scan::{scan_session_candidates_once, CaptureSessionContext};
use crate::provider::SessionId;

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
fn pi_fresh_capture_reads_the_header_id_pi_wrote() {
    let root = temp_root("native-header");
    let cwd = root.join("workspace");
    let sessions = root.join("seat/sessions");
    std::fs::create_dir_all(&cwd).expect("create cwd");
    let native_id = "410271aa-2a95-46fe-a92e-41927688fb0c";
    let path = sessions.join("pi-created.jsonl");
    write_header(&path, native_id, &cwd, "2026-08-29T10:00:01Z");
    let mut context = scan_context(&cwd, &sessions, "unused", "2026-08-29T10:00:00Z");
    context.expected_session_id = None;

    let candidates = scan_session_candidates_once(Provider::Pi, &context).expect("native header scan");
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].captured.session_id.as_ref().map(SessionId::as_str),
        Some(native_id)
    );
    std::fs::remove_dir_all(root).expect("remove native header fixture");
}
