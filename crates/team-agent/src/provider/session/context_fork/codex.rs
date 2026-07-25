use super::*;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

#[derive(Debug)]
pub(crate) struct CodexForkMaterialization {
    path: PathBuf,
    keep: bool,
}

impl CodexForkMaterialization {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn mark_handoff(&mut self) {
        self.keep = true;
    }
}

impl crate::provider::adapter::ForkBackingMaterialization for CodexForkMaterialization {
    fn path(&self) -> &Path {
        self.path()
    }

    fn handoff(&mut self) {
        self.mark_handoff();
    }
}

impl Drop for CodexForkMaterialization {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn materialize_codex_fork(
    source_path: &Path,
    source_session_id: &SessionId,
    source_agent_id: &str,
    target_agent_id: &str,
    plan: &mut CommandPlan,
) -> Result<CodexForkMaterialization, ProviderError> {
    let target_session_id = SessionId::new(codex_session_v7());
    let parent = source_path.parent().ok_or_else(|| {
        ProviderError::Io(format!(
            "codex source backing has no parent: {}",
            source_path.display()
        ))
    })?;
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S");
    let target_path = parent.join(format!(
        "rollout-{timestamp}-{}.jsonl",
        target_session_id.as_str()
    ));
    let source = complete_source_snapshot(source_path)?;
    let rewritten = rewrite_snapshot(
        &source,
        source_session_id,
        &target_session_id,
        source_agent_id,
        target_agent_id,
    )?;
    publish_target_no_clobber(&target_path, rewritten.as_bytes())?;
    if let Err(error) =
        validate_materialized_target(&target_path, &target_session_id, target_agent_id)
    {
        let _ = std::fs::remove_file(&target_path);
        return Err(error);
    }
    apply_resume_target(plan, source_session_id, &target_session_id)?;
    Ok(CodexForkMaterialization {
        path: target_path,
        keep: false,
    })
}

fn complete_source_snapshot(path: &Path) -> Result<String, ProviderError> {
    let file = File::open(path).map_err(|error| ProviderError::Io(error.to_string()))?;
    let visible_len = file
        .metadata()
        .map_err(|error| ProviderError::Io(error.to_string()))?
        .len();
    let mut bytes = Vec::new();
    file.take(visible_len)
        .read_to_end(&mut bytes)
        .map_err(|error| ProviderError::Io(error.to_string()))?;
    let Some(boundary) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Err(ProviderError::Io(
            "codex fork source has no complete JSONL record".to_string(),
        ));
    };
    bytes.truncate(boundary + 1);
    String::from_utf8(bytes).map_err(|error| ProviderError::Io(error.to_string()))
}

fn rewrite_snapshot(
    source: &str,
    source_session_id: &SessionId,
    target_session_id: &SessionId,
    source_agent_id: &str,
    target_agent_id: &str,
) -> Result<String, ProviderError> {
    let source_marker = format!("You are Team Agent worker `{source_agent_id}`");
    let target_marker = format!("You are Team Agent worker `{target_agent_id}`");
    let mut meta_count = 0_usize;
    let mut marker_count = 0_usize;
    let mut output = String::new();
    for (index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut record = serde_json::from_str::<serde_json::Value>(line).map_err(|error| {
            ProviderError::Io(format!(
                "codex fork source has invalid JSONL at line {}: {error}",
                index + 1
            ))
        })?;
        if record.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
            let id = record
                .get_mut("payload")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|payload| payload.get_mut("id"))
                .and_then(|id| id.as_str())
                .ok_or_else(|| {
                    ProviderError::Io(
                        "codex fork session_meta has no string payload.id".to_string(),
                    )
                })?;
            if id != source_session_id.as_str() {
                return Err(ProviderError::Io(
                    "codex fork session_meta id does not match source".to_string(),
                ));
            }
            record["payload"]["id"] =
                serde_json::Value::String(target_session_id.as_str().to_string());
            meta_count += 1;
        }
        marker_count += replace_exact_marker(&mut record, &source_marker, &target_marker);
        output.push_str(
            &serde_json::to_string(&record)
                .map_err(|error| ProviderError::Io(error.to_string()))?,
        );
        output.push('\n');
    }
    if meta_count != 1 || marker_count != 1 {
        return Err(ProviderError::Io(format!(
            "codex fork source identity is ambiguous: session_meta={meta_count}, marker={marker_count}"
        )));
    }
    Ok(output)
}

fn replace_exact_marker(value: &mut serde_json::Value, source: &str, target: &str) -> usize {
    match value {
        serde_json::Value::String(text) => {
            let count = text.matches(source).count();
            if count == 1 {
                *text = text.replacen(source, target, 1);
            }
            count
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|item| replace_exact_marker(item, source, target))
            .sum(),
        serde_json::Value::Object(fields) => fields
            .values_mut()
            .map(|item| replace_exact_marker(item, source, target))
            .sum(),
        _ => 0,
    }
}

fn publish_target_no_clobber(path: &Path, bytes: &[u8]) -> Result<(), ProviderError> {
    let parent = path.parent().ok_or_else(|| {
        ProviderError::Io(format!("codex target has no parent: {}", path.display()))
    })?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("codex-fork"),
        std::process::id()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::hard_link(&temp, path)?;
        File::open(parent)?.sync_all()?;
        std::fs::remove_file(&temp)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(ProviderError::Io(error.to_string()));
    }
    Ok(())
}

fn validate_materialized_target(
    path: &Path,
    session_id: &SessionId,
    target_agent_id: &str,
) -> Result<(), ProviderError> {
    let text =
        std::fs::read_to_string(path).map_err(|error| ProviderError::Io(error.to_string()))?;
    let actual = session_id_from_jsonl(path);
    let marker = format!("You are Team Agent worker `{target_agent_id}`");
    if actual.as_deref() != Some(session_id.as_str())
        || !path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(session_id.as_str()))
        || text.matches(&marker).count() != 1
    {
        return Err(ProviderError::Io(
            "codex fork target read-back identity mismatch".to_string(),
        ));
    }
    Ok(())
}

fn apply_resume_target(
    plan: &mut CommandPlan,
    source_session_id: &SessionId,
    target_session_id: &SessionId,
) -> Result<(), ProviderError> {
    if plan.argv.first().map(String::as_str) != Some("codex")
        || plan.argv.get(1).map(String::as_str) != Some("fork")
        || plan.argv.last().map(String::as_str) != Some(source_session_id.as_str())
    {
        return Err(ProviderError::Command(
            "codex fork command shape cannot be converted to exact resume".to_string(),
        ));
    }
    plan.argv[1] = "resume".to_string();
    *plan.argv.last_mut().expect("validated non-empty argv") =
        target_session_id.as_str().to_string();
    plan.expected_session_id = Some(target_session_id.clone());
    Ok(())
}

fn codex_session_v7() -> String {
    use sha2::Digest;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    let mut hasher = sha2::Sha256::new();
    hasher.update(millis.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(
        COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_le_bytes(),
    );
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest[..10]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[4..6].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[6..8].try_into().expect("fixed slice")),
        u16::from_be_bytes(bytes[8..10].try_into().expect("fixed slice")),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    )
}

pub(super) fn verify_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    expected_backing_path: Option<&Path>,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let Some(expected) = plan.expected_session_id.as_ref() else {
        return verify_legacy_codex_fork(
            source_session_id,
            plan,
            before,
            agent_id,
            spawn_cwd,
            spawned_at,
            deadline,
        );
    };
    if expected == source_session_id {
        return Err(ProviderError::CaptureFailed(
            "context_fork_unverified: codex target session equals source".to_string(),
        )
        .into());
    }
    let expected_path = expected_backing_path.ok_or_else(|| {
        ProviderError::CaptureFailed(
            "context_fork_unverified: codex plan has no materialized target backing".to_string(),
        )
    })?;
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
        for candidate in
            crate::provider::session_scan::scan_session_candidates_once(Provider::Codex, &context)?
        {
            let Some(path) = candidate.captured.rollout_path.as_ref() else {
                continue;
            };
            if path.as_path() != expected_path {
                continue;
            }
            let Some(new_session_id) = candidate.captured.session_id else {
                continue;
            };
            if &new_session_id != expected || &new_session_id == source_session_id {
                continue;
            }
            if !candidate.positive_agent_id_match
                || candidate.embedded_agent_id.as_deref() != Some(agent_id)
            {
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
    Err(ContextForkTermination::Timeout {
        provider: Provider::Codex,
        deadline_ms: deadline.as_millis(),
    })
}

fn verify_legacy_codex_fork(
    source_session_id: &SessionId,
    plan: &CommandPlan,
    before: &ContextBackingSnapshot,
    agent_id: &str,
    spawn_cwd: &Path,
    spawned_at: &str,
    deadline: Duration,
) -> Result<ContextForkProof, ContextForkTermination> {
    let context = crate::provider::session_scan::CaptureSessionContext {
        agent_id: agent_id.to_string(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        pane_id: None,
        pane_pid: None,
        spawned_at: Some(spawned_at.to_string()),
        expected_session_id: None,
        provider_projects_root: plan.provider_projects_root.clone(),
    };
    let started = std::time::Instant::now();
    loop {
        let current = jsonl_files(&provider_backing_root(Provider::Codex, plan));
        let mut matches = Vec::new();
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
            let Some(session_id) = candidate.captured.session_id else {
                continue;
            };
            if &session_id == source_session_id {
                continue;
            }
            matches.push((session_id, path.as_path().to_path_buf()));
        }
        if let [(new_session_id, backing_path)] = matches.as_slice() {
            return Ok(ContextForkProof {
                provider: Provider::Codex,
                source_session_id: source_session_id.clone(),
                new_session_id: new_session_id.clone(),
                backing_path: backing_path.clone(),
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
    Err(ContextForkTermination::Timeout {
        provider: Provider::Codex,
        deadline_ms: deadline.as_millis(),
    })
}
