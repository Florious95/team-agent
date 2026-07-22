use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::enums::Provider;
use crate::provider::{CommandPlan, ProviderError, SessionId};

pub(crate) fn context_fork_convergence_deadline(provider: Provider) -> Duration {
    match provider {
        Provider::Claude | Provider::ClaudeCode => Duration::from_secs(45),
        Provider::Codex => Duration::from_secs(10),
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => Duration::from_secs(5),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextForkProof {
    pub provider: Provider,
    pub source_session_id: SessionId,
    pub new_session_id: SessionId,
    pub backing_path: PathBuf,
    pub captured_via: String,
    pub attribution_confidence: String,
    pub managed_backing_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextBackingSnapshot {
    root: PathBuf,
    files: BTreeMap<PathBuf, FileStamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl ContextBackingSnapshot {
    pub(crate) fn capture(provider: Provider, plan: &CommandPlan) -> Self {
        let root = provider_backing_root(provider, plan);
        let files = jsonl_files(&root);
        Self { root, files }
    }
}

pub(crate) fn verify_context_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ProviderError> {
    if provider == Provider::Copilot {
        return verify_copilot_fork(source_session_id, plan);
    }
    if provider == Provider::Codex {
        return verify_codex_fork(
            source_session_id,
            plan,
            before,
            agent_id,
            spawn_cwd,
            spawned_at,
            deadline,
        );
    }
    if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return verify_claude_fork(
            provider,
            source_session_id,
            plan,
            before,
            expected_backing_path,
            deadline,
        );
    }
    Err(ProviderError::CaptureFailed(format!(
        "context_fork_unverified: {provider:?} has no verifiable fork backing"
    )))
}
fn verify_claude_fork(
    provider: Provider,
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    deadline: Duration,
) -> Result<ContextForkProof, ProviderError> {
    let expected = plan.expected_session_id.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no expected session id".to_string(),
        )
    })?;
    let path = expected_backing_path.ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: Claude plan has no exact snapshot backing".to_string(),
        )
    })?;
    let expected_name = format!("{}.jsonl", expected.as_str());
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(ProviderError::CaptureFailed(format!(
            "context_fork_unverified: Claude snapshot backing does not match expected session {}",
            expected.as_str()
        )));
    }
    let started = std::time::Instant::now();
    loop {
        if let Some(stamp) = file_stamp(path) {
            let changed = before.files.get(path).is_none_or(|old| *old != stamp);
            let observed_matches =
                session_id_from_jsonl(path).is_none_or(|observed| observed == expected.as_str());
            if changed && readable_jsonl(path) && observed_matches && expected != source_session_id
            {
                return Ok(ContextForkProof {
                    provider,
                    source_session_id: source_session_id.clone(),
                    new_session_id: expected.clone(),
                    backing_path: path.to_path_buf(),
                    captured_via: "context_fork_verified".to_string(),
                    attribution_confidence: "high".to_string(),
                    managed_backing_root: None,
                });
            }
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ProviderError::CaptureFailed(format!(
        "context_fork_unverified: {provider:?} produced no readable NEW session backing within {}ms",
        deadline.as_millis()
    )))
}

fn verify_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ProviderError> {
    let context = crate::provider::session_scan::CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(spawned_at.to_string()),
        expected_session_id: plan.expected_session_id.clone(),
        provider_projects_root: plan.provider_projects_root.clone(),
    };
    let started = std::time::Instant::now();
    loop {
        let current = jsonl_files(&before.root);
        for candidate in
            crate::provider::session_scan::scan_session_candidates_once(Provider::Codex, &context)?
        {
            let Some(path) = candidate.captured.rollout_path.as_ref() else {
                continue;
            };
            let Some(stamp) = current.get(path.as_path()) else {
                continue;
            };
            if before.files.get(path.as_path()) == Some(stamp) {
                continue;
            }
            let Some(new_session_id) = candidate.captured.session_id else {
                continue;
            };
            if new_session_id == *source_session_id {
                continue;
            }
            return Ok(ContextForkProof {
                provider: Provider::Codex,
                source_session_id: source_session_id.clone(),
                new_session_id,
                backing_path: path.as_path().to_path_buf(),
                captured_via: "context_fork_verified".to_string(),
                attribution_confidence: "high".to_string(),
                managed_backing_root: None,
            });
        }
        if started.elapsed() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ProviderError::CaptureFailed(format!(
        "context_fork_unverified: Codex produced no readable NEW session backing within {}ms",
        deadline.as_millis()
    )))
}

fn verify_copilot_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
) -> Result<ContextForkProof, ProviderError> {
    let expected = plan.expected_session_id.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: copilot plan has no expected session id".to_string(),
        )
    })?;
    if expected == source_session_id {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: copilot NEW session equals source".to_string(),
        ));
    }
    let root = plan.provider_projects_root.as_ref().ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: copilot plan has no isolated backing root".to_string(),
        )
    })?;
    let state_dir = root.join("session-state").join(expected.as_str());
    let db_path = root.join("session-store.db");
    if !state_dir.is_dir() || !db_path.is_file() {
        return Err(ProviderError::CaptureFailed(format!(
            "context_fork_unverified: copilot backing missing under {}",
            root.display()
        )));
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| ProviderError::CaptureFailed(error.to_string()))?;
    let found: i64 = conn
        .query_row(
            "select count(*) from sessions where id = ?1",
            [expected.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| ProviderError::CaptureFailed(error.to_string()))?;
    if found != 1 {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: copilot NEW session is absent from session-store".to_string(),
        ));
    }
    Ok(ContextForkProof {
        provider: Provider::Copilot,
        source_session_id: source_session_id.clone(),
        new_session_id: expected.clone(),
        backing_path: db_path,
        captured_via: "copilot_store_fork_verified".to_string(),
        attribution_confidence: "high".to_string(),
        managed_backing_root: Some(root.clone()),
    })
}

fn provider_backing_root(provider: Provider, plan: &CommandPlan) -> PathBuf {
    if let Some(root) = plan.provider_projects_root.as_ref() {
        return root.clone();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    match provider {
        Provider::Claude | Provider::ClaudeCode => home.join(".claude").join("projects"),
        Provider::Codex => home.join(".codex").join("sessions"),
        Provider::Copilot => home.join(".copilot").join("session-state"),
        Provider::GeminiCli | Provider::Fake => home,
    }
}

fn jsonl_files(root: &Path) -> BTreeMap<PathBuf, FileStamp> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                if let Some(stamp) = file_stamp(&path) {
                    files.insert(path, stamp);
                }
            }
        }
    }
    files
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn readable_jsonl(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_string)
        })
        .is_some_and(|line| serde_json::from_str::<serde_json::Value>(&line).is_ok())
}

fn session_id_from_jsonl(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|record| {
            record
                .get("sessionId")
                .or_else(|| record.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    record
                        .get("session_meta")
                        .and_then(|value| value.get("payload"))
                        .or_else(|| record.get("payload"))
                        .and_then(|value| value.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn context_fork_deadlines_are_provider_specific() {
        assert_eq!(
            context_fork_convergence_deadline(Provider::Claude),
            Duration::from_secs(45)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Codex),
            Duration::from_secs(10)
        );
        assert_eq!(
            context_fork_convergence_deadline(Provider::Copilot),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn codex_fork_proof_ignores_changed_stale_and_foreign_rollouts() {
        let root =
            std::env::temp_dir().join(format!("ta-codex-fork-proof-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let current_cwd = root.join("current");
        let foreign_cwd = root.join("foreign");
        std::fs::create_dir_all(&current_cwd).unwrap();
        std::fs::create_dir_all(&foreign_cwd).unwrap();
        let stale = root.join(
            "2026/07/16/rollout-2026-07-16T08-05-43-019f6686-e2b6-7000-8000-000000000001.jsonl",
        );
        let fresh = root.join(
            "2026/07/22/rollout-2026-07-22T04-01-01-019f8644-7450-7000-8000-000000000002.jsonl",
        );
        let foreign = root.join(
            "2026/07/22/rollout-2026-07-22T04-01-02-019f8644-7838-7000-8000-000000000003.jsonl",
        );
        let write_rollout = |path: &Path, cwd: &Path, id: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{}\"}}}}\n",
                    cwd.to_string_lossy()
                ),
            )
            .unwrap();
        };
        write_rollout(&stale, &current_cwd, "stale-session");
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: None,
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Codex, &plan);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&stale)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\"}\n")
            .unwrap();
        write_rollout(&foreign, &foreign_cwd, "foreign-session");
        write_rollout(&fresh, &current_cwd, "fresh-session");

        let proof = verify_context_fork(
            Provider::Codex,
            &SessionId::new("source-session"),
            &plan,
            &before,
            None,
            "fork",
            &current_cwd,
            "2026-07-21T20:00:00+00:00",
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(proof.new_session_id.as_str(), "fresh-session");
        assert_eq!(proof.backing_path, fresh);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_fork_proof_is_partitioned_by_exact_snapshot_path() {
        let root = std::env::temp_dir().join(format!(
            "ta-claude-fork-proof-partition-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".team/logs")).unwrap();
        let expected = SessionId::new("22222222-2222-4222-8222-222222222222");
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: Some(expected.clone()),
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Claude, &plan);
        std::fs::write(
            root.join(".team/logs/events.jsonl"),
            "{\"event\":\"noise\"}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("11111111-1111-4111-8111-111111111111.jsonl"),
            "{\"type\":\"sibling-snapshot\"}\n",
        )
        .unwrap();
        let expected_path = root.join(format!("{}.jsonl", expected.as_str()));
        std::fs::write(&expected_path, "{\"type\":\"own-snapshot\"}\n").unwrap();
        let proof = verify_context_fork(
            Provider::Claude,
            &SessionId::new("source-session"),
            &plan,
            &before,
            Some(&expected_path),
            "fork",
            &root,
            "2026-07-22T00:00:00Z",
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(proof.new_session_id, expected);
        assert_eq!(proof.backing_path, expected_path);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn claude_fork_proof_never_uses_events_jsonl_as_backing() {
        let root = std::env::temp_dir().join(format!(
            "ta-claude-fork-proof-no-events-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".team/logs")).unwrap();
        let expected = SessionId::new("33333333-3333-4333-8333-333333333333");
        let plan = CommandPlan {
            argv: Vec::new(),
            expected_session_id: Some(expected.clone()),
            provider_projects_root: Some(root.clone()),
            managed_mcp_config: false,
        };
        let before = ContextBackingSnapshot::capture(Provider::Claude, &plan);
        std::fs::write(
            root.join(".team/logs/events.jsonl"),
            "{\"event\":\"noise\"}\n",
        )
        .unwrap();
        let missing = root.join(format!("{}.jsonl", expected.as_str()));
        let error = verify_context_fork(
            Provider::Claude,
            &SessionId::new("source-session"),
            &plan,
            &before,
            Some(&missing),
            "fork",
            &root,
            "2026-07-22T00:00:00Z",
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.to_string().contains("context_fork_unverified"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
