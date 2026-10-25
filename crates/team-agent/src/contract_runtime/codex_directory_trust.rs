//! Host-side directory trust for a contract Codex seat.
//!
//! The contract leaf forbids global config writes. Launch still has to record
//! trust for the seat's git root or the native folder-trust modal blocks the
//! frame. This module is the neutral host helper previously borrowed from the
//! deleted legacy adapter.

use std::path::{Path, PathBuf};

/// Git root of `cwd`. Codex records directory trust on that absolute path.
pub(crate) fn git_work_root(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = PathBuf::from(text.trim());
    root.is_absolute().then_some(root)
}

pub(crate) fn codex_home_dir() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".codex"))
        })
}

/// Trust this cwd's git root inside the worker Codex home. Does not send keys.
pub(crate) fn ensure_worker_directory_trust(
    codex_home: &Path,
    git_root: &Path,
) -> std::io::Result<()> {
    if !git_root.is_absolute() {
        return Ok(());
    }
    let root = git_root.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "git root is not utf-8")
    })?;
    if root.contains('"') || root.contains('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "git root cannot be quoted into the trust table",
        ));
    }
    std::fs::create_dir_all(codex_home)?;
    let path = codex_home.join("config.toml");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let header = format!("[projects.\"{root}\"]");
    if existing.contains(&header) {
        return Ok(());
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.contains("check_for_update_on_startup") {
        next.push_str("check_for_update_on_startup = false\n");
    }
    next.push('\n');
    next.push_str(&header);
    next.push_str("\ntrust_level = \"trusted\"\n");
    let tmp = codex_home.join("config.toml.tmp");
    std::fs::write(&tmp, next)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod trust_file_tests {
    use super::ensure_worker_directory_trust;

    #[test]
    fn trust_file_records_git_root_once() {
        let dir = std::env::temp_dir().join(format!("codex-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = std::path::Path::new("/Volumes/nvme/tmp/example-root");
        ensure_worker_directory_trust(&dir, root).unwrap();
        ensure_worker_directory_trust(&dir, root).unwrap();
        let text = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert_eq!(
            text,
            "check_for_update_on_startup = false\n\n[projects.\"/Volumes/nvme/tmp/example-root\"]\ntrust_level = \"trusted\"\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
