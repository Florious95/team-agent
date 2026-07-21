use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::model::enums::Provider;
use crate::provider::{CommandPlan, ProviderError, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackingConvergenceOperation {
    Clone,
    Fork,
}

pub(crate) fn backing_convergence_deadline(
    provider: Provider,
    operation: BackingConvergenceOperation,
) -> Duration {
    match (provider, operation) {
        (Provider::Claude | Provider::ClaudeCode, _) => Duration::from_secs(45),
        (Provider::Codex | Provider::Copilot, BackingConvergenceOperation::Clone) => {
            Duration::from_secs(30)
        }
        (Provider::Codex, BackingConvergenceOperation::Fork) => Duration::from_secs(10),
        (Provider::Copilot, BackingConvergenceOperation::Fork) => Duration::from_secs(5),
        (Provider::GeminiCli | Provider::Fake, _) => Duration::from_secs(5),
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
    deadline: Duration,
) -> Result<ContextForkProof, ProviderError> {
    if provider == Provider::Copilot {
        return verify_copilot_fork(source_session_id, plan);
    }
    let started = std::time::Instant::now();
    loop {
        for (path, stamp) in jsonl_files(&before.root) {
            let changed = before.files.get(&path).is_none_or(|old| *old != stamp);
            if !changed || !readable_jsonl(&path) {
                continue;
            }
            let observed = session_id_from_jsonl(&path);
            let new_session_id = match (&plan.expected_session_id, observed) {
                (Some(expected), Some(observed)) if observed != expected.as_str() => continue,
                (Some(expected), _) => expected.clone(),
                (None, Some(observed)) => SessionId::new(observed),
                (None, None) => continue,
            };
            if new_session_id == *source_session_id {
                continue;
            }
            return Ok(ContextForkProof {
                provider,
                source_session_id: source_session_id.clone(),
                new_session_id,
                backing_path: path,
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
        "context_fork_unverified: {provider:?} produced no readable NEW session backing within {}ms",
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
                if let Ok(meta) = entry.metadata() {
                    files.insert(
                        path,
                        FileStamp {
                            len: meta.len(),
                            modified: meta.modified().ok(),
                        },
                    );
                }
            }
        }
    }
    files
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

    #[test]
    fn convergence_deadlines_are_provider_and_operation_specific() {
        assert_eq!(
            backing_convergence_deadline(Provider::Claude, BackingConvergenceOperation::Clone),
            Duration::from_secs(45)
        );
        assert_eq!(
            backing_convergence_deadline(Provider::Codex, BackingConvergenceOperation::Clone),
            Duration::from_secs(30)
        );
        assert_eq!(
            backing_convergence_deadline(Provider::Codex, BackingConvergenceOperation::Fork),
            Duration::from_secs(10)
        );
        assert_eq!(
            backing_convergence_deadline(Provider::Copilot, BackingConvergenceOperation::Fork),
            Duration::from_secs(5)
        );
    }
}
