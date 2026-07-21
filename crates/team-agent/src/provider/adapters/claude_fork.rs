use std::path::{Path, PathBuf};

use crate::provider::{ProviderError, SessionId};

#[derive(Debug)]
pub(crate) struct ClaudeForkMaterialization {
    path: PathBuf,
    keep: bool,
}

impl ClaudeForkMaterialization {
    pub(crate) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for ClaudeForkMaterialization {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn materialize_claude_fork(
    source_path: &Path,
    source_session_id: &SessionId,
    target_session_id: &SessionId,
) -> Result<ClaudeForkMaterialization, ProviderError> {
    let parent = source_path.parent().ok_or_else(|| {
        ProviderError::Io(format!(
            "claude source backing has no parent: {}",
            source_path.display()
        ))
    })?;
    let target_path = parent.join(format!("{}.jsonl", target_session_id.as_str()));
    if target_path.exists() {
        return Err(ProviderError::Io(format!(
            "claude fork destination already exists: {}",
            target_path.display()
        )));
    }
    let source = std::fs::read_to_string(source_path)
        .map_err(|error| ProviderError::Io(error.to_string()))?;
    let rewritten = source.replace(source_session_id.as_str(), target_session_id.as_str());
    validate_jsonl(&rewritten)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        target_session_id.as_str(),
        std::process::id()
    ));
    let result = std::fs::write(&temp, rewritten)
        .and_then(|_| std::fs::rename(&temp, &target_path))
        .map_err(|error| ProviderError::Io(error.to_string()));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(ClaudeForkMaterialization {
        path: target_path,
        keep: false,
    })
}

fn validate_jsonl(text: &str) -> Result<(), ProviderError> {
    let mut rows = 0_usize;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(line).map_err(|error| {
            ProviderError::Io(format!(
                "claude fork source has invalid JSONL at line {}: {error}",
                index + 1
            ))
        })?;
        rows += 1;
    }
    if rows == 0 {
        return Err(ProviderError::Io(
            "claude fork source backing has no JSONL records".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_rekeys_every_record_and_preserves_source() {
        let dir = std::env::temp_dir().join(format!("ta-claude-fork-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source_id = SessionId::new("11111111-1111-4111-8111-111111111111");
        let target_id = SessionId::new("22222222-2222-4222-8222-222222222222");
        let source_path = dir.join(format!("{}.jsonl", source_id.as_str()));
        let source = format!(
            "{{\"sessionId\":\"{}\"}}\n{{\"payload\":{{\"session_id\":\"{}\"}}}}\n",
            source_id.as_str(),
            source_id.as_str()
        );
        std::fs::write(&source_path, &source).unwrap();

        let mut materialized =
            materialize_claude_fork(&source_path, &source_id, &target_id).unwrap();
        let target_path = dir.join(format!("{}.jsonl", target_id.as_str()));
        let target = std::fs::read_to_string(&target_path).unwrap();
        assert!(!target.contains(source_id.as_str()));
        assert!(target.contains(target_id.as_str()));
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), source);
        materialized.keep();
        drop(materialized);
        assert!(target_path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
