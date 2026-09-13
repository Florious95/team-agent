use super::*;

#[derive(Default, Debug)]
struct EnumerationCounts {
    directories: Vec<PathBuf>,
    entries: usize,
    following_metadata: usize,
}

thread_local! {
    static ENUMERATION: std::cell::RefCell<EnumerationCounts> =
        std::cell::RefCell::new(EnumerationCounts::default());
}

pub(super) fn record_directory_read(path: &Path) {
    ENUMERATION.with(|counts| counts.borrow_mut().directories.push(path.to_path_buf()));
}

pub(super) fn record_entry() {
    ENUMERATION.with(|counts| counts.borrow_mut().entries += 1);
}

pub(super) fn record_following_metadata() {
    ENUMERATION.with(|counts| counts.borrow_mut().following_metadata += 1);
}

fn take_counts() -> EnumerationCounts {
    ENUMERATION.with(|counts| std::mem::take(&mut *counts.borrow_mut()))
}

// The previous traversal boundary, run only on the synthetic fixture. Count
// explicit metadata calls, not inferred OS syscalls or elapsed-time thresholds.
fn legacy_enumeration_counts(dir: &Path, depth: usize, counts: &mut EnumerationCounts) {
    if depth > 4 {
        return;
    }
    counts.directories.push(dir.to_path_buf());
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        counts.entries += 1;
        counts.following_metadata += 1;
        if path.is_dir() {
            legacy_enumeration_counts(&path, depth + 1, counts);
        }
    }
}

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

#[test]
fn repeated_wide_directory_scans_remove_leaf_metadata_without_hiding_new_files() {
    let dir = temp_dir("wide-discovery");
    for group in 0..4 {
        let child = dir.join("source").join(format!("group-{group}"));
        std::fs::create_dir_all(&child).unwrap();
        for index in 0..128 {
            std::fs::write(child.join(format!("code-{index}.rs")), "").unwrap();
        }
    }
    for _ in 0..2 {
        let mut before = EnumerationCounts::default();
        legacy_enumeration_counts(&dir, 0, &mut before);
        assert_eq!((before.directories.len(), before.entries), (6, 517));
        assert_eq!(before.following_metadata, 517);

        take_counts();
        let mut candidates = Vec::new();
        collect_candidate_files(&dir, "worker", 0, false, None, &mut candidates).unwrap();
        let after = take_counts();
        assert!(candidates.is_empty());
        assert_eq!((after.directories.len(), after.entries), (6, 517));
        assert_eq!(after.following_metadata, 0);
    }
    // No .team subtree is needed to reproduce the redundant metadata work.
    // A negative scan must not suppress a later file in the same nested cwd.
    let rollout = dir.join("source/group-3/rollout-worker.jsonl");
    write_codex_rollout(&rollout, &dir, "late-worker", "worker");
    let mut candidates = Vec::new();
    collect_candidate_files(&dir, "worker", 0, false, None, &mut candidates).unwrap();
    let after = take_counts();
    assert_eq!((after.directories.len(), after.entries), (6, 518));
    assert_eq!(after.following_metadata, 0);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, rollout);
    assert!(!candidates[0].requires_cwd_match);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn team_subtrees_are_pruned_before_enumeration_but_claude_root_and_ancestors_survive() {
    let dir = temp_dir("team-pruning");
    let allowed = dir.join(".team/runtime/claude/projects");
    let owned = allowed.join("cwd/owned.jsonl");
    let unrelated = dir.join(".team/artifacts/deep/ignored.jsonl");
    let sibling = dir.join(".team/runtime/other/ignored.jsonl");
    for path in [&owned, &unrelated, &sibling] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }

    let mut before = EnumerationCounts::default();
    legacy_enumeration_counts(&dir, 0, &mut before);
    assert_eq!((before.directories.len(), before.entries), (8, 10));
    assert_eq!(before.following_metadata, 10);

    take_counts();
    let mut candidates = Vec::new();
    collect_candidate_files(&dir, "worker", 0, false, None, &mut candidates).unwrap();
    let counts = take_counts();
    assert!(candidates.is_empty());
    assert_eq!(counts.directories, vec![dir.clone()]);
    assert_eq!((counts.entries, counts.following_metadata), (1, 0));

    // Starting at the runtime ancestor must reach the explicit Claude root,
    // while siblings inside .team remain excluded before read_dir.
    let ancestor = dir.join(".team");
    collect_candidate_files(&ancestor, "worker", 0, true, Some(&allowed), &mut candidates)
        .unwrap();
    let counts = take_counts();
    assert_eq!(counts.directories.len(), 5);
    assert_eq!((counts.entries, counts.following_metadata), (7, 0));
    assert!(!counts
        .directories
        .iter()
        .any(|path| path.starts_with(dir.join(".team/artifacts"))));
    assert!(!counts
        .directories
        .iter()
        .any(|path| path.starts_with(dir.join(".team/runtime/other"))));
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, owned);
    assert!(candidates[0].requires_cwd_match);

    candidates.clear();
    collect_optional_candidate_files(&allowed, "worker", Some(&allowed), &mut candidates)
        .unwrap();
    assert_eq!(take_counts().directories.len(), 2);
    assert_eq!(candidates.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn traversal_keeps_depth_limit_and_required_root_errors() {
    let dir = temp_dir("depth-limit");
    let at_limit = dir.join("one/two/three/four/at-limit.jsonl");
    let beyond = dir.join("one/two/three/four/five/beyond.jsonl");
    std::fs::create_dir_all(beyond.parent().unwrap()).unwrap();
    std::fs::write(&at_limit, "{}").unwrap();
    std::fs::write(&beyond, "{}").unwrap();
    take_counts();
    let mut candidates = Vec::new();
    collect_candidate_files(&dir, "worker", 0, false, None, &mut candidates).unwrap();
    let counts = take_counts();
    assert_eq!(counts.directories.len(), 5);
    assert_eq!(counts.following_metadata, 0);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, at_limit);
    assert!(collect_candidate_files(
        &dir.join("missing"),
        "worker",
        0,
        false,
        None,
        &mut Vec::new()
    )
    .is_err());
    assert!(collect_optional_candidate_files(
        &dir.join("missing"),
        "worker",
        None,
        &mut Vec::new()
    )
    .is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(unix)]
fn symlink_files_directories_and_cycles_keep_following_metadata_and_depth_semantics() {
    use std::os::unix::fs::symlink;
    let dir = temp_dir("symlink-discovery");
    let target = dir.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("session.jsonl"), "{}").unwrap();
    symlink(&target, dir.join("linked-dir")).unwrap();
    symlink(target.join("session.jsonl"), dir.join("linked-file.jsonl")).unwrap();
    symlink(dir.join("absent"), dir.join("dangling.jsonl")).unwrap();
    symlink(&target, target.join("cycle")).unwrap();
    take_counts();
    let mut candidates = Vec::new();
    collect_candidate_files(&dir, "worker", 0, false, None, &mut candidates).unwrap();
    let counts = take_counts();
    assert_eq!(counts.directories.len(), 9);
    assert_eq!(counts.entries, 20);
    assert_eq!(counts.following_metadata, 11);
    assert_eq!(candidates.len(), 10);
    assert!(candidates
        .iter()
        .any(|item| item.path == dir.join("linked-file.jsonl")));
    assert!(candidates
        .iter()
        .any(|item| item.path == dir.join("linked-dir/session.jsonl")));
    assert!(candidates
        .iter()
        .any(|item| item.path == dir.join("dangling.jsonl")));
    std::fs::remove_dir_all(dir).unwrap();
}
