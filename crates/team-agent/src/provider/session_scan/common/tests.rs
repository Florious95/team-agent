use super::*;

fn temp_dir(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("ta-session-scan-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_codex_rollout(path: &Path, cwd: &Path, session_id: &str, embedded_agent_id: &str) {
    let text = format!(
        "{{\"session_meta\":{{\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\"}}}}}}\n\
         {{\"type\":\"turn_context\",\"payload\":{{}}}}\n\
         {{\"type\":\"response_item\",\"payload\":{{\"content\":[{{\"type\":\"input_text\",\"text\":\"You are Team Agent worker `{embedded_agent_id}` with role `fixture`.\"}}]}}}}\n",
        cwd.to_string_lossy()
    );
    std::fs::write(path, text).expect("write codex rollout");
}

#[test]
fn codex_prompt_worker_identity_is_a_positive_match() {
    let dir = temp_dir("positive");
    let rollout = dir.join("rollout-frontend.jsonl");
    write_codex_rollout(&rollout, &dir, "sess-frontend", "frontend");
    let context = CaptureSessionContext {
        agent_id: "frontend".to_string(),
        spawn_cwd: dir.clone(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: None,
        provider_projects_root: None,
    };
    let candidates = parse_candidate_files(
        Provider::Codex,
        &context,
        vec![SessionCandidate {
            path: rollout.clone(),
            requires_cwd_match: false,
        }],
    );
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].positive_agent_id_match);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_identity_in_incomplete_final_head_record_is_positive() {
    let dir = temp_dir("cross-cap-positive");
    let rollout = dir.join("rollout-worker-a.jsonl");
    let meta = format!(
        "{{\"session_meta\":{{\"payload\":{{\"id\":\"sess-worker-a\",\"cwd\":\"{}\"}}}}}}\n",
        dir.to_string_lossy()
    );
    let marker = "You are Team Agent worker `worker-a` with role `fixture`.";
    let body = format!("{meta}{{\"text\":\"{marker}{}\"}}\n", "x".repeat(70_000));
    std::fs::write(&rollout, body).expect("write cross-cap rollout");
    assert!(!read_head_text(&rollout, CAPTURE_HEAD_BYTES)
        .expect("read complete records")
        .contains(marker));
    let context = CaptureSessionContext {
        agent_id: "worker-a".to_string(),
        spawn_cwd: dir.clone(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: None,
        provider_projects_root: None,
    };

    let candidates = parse_candidate_files(
        Provider::Codex,
        &context,
        vec![SessionCandidate {
            path: rollout,
            requires_cwd_match: false,
        }],
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].embedded_agent_id.as_deref(), Some("worker-a"));
    assert!(candidates[0].positive_agent_id_match);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_prompt_worker_identity_mismatch_is_rejected_for_that_agent() {
    let dir = temp_dir("mismatch");
    let rollout = dir.join("rollout-ios-dev.jsonl");
    write_codex_rollout(&rollout, &dir, "sess-ios-dev", "ios-dev");
    let context = CaptureSessionContext {
        agent_id: "frontend".to_string(),
        spawn_cwd: dir.clone(),
        pane_id: None,
        pane_pid: None,
        spawned_at: None,
        expected_session_id: None,
        provider_projects_root: None,
    };
    let candidates = parse_candidate_files(
        Provider::Codex,
        &context,
        vec![SessionCandidate {
            path: rollout,
            requires_cwd_match: false,
        }],
    );
    assert!(candidates.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
