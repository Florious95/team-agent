//! Host-global supplemental text, read only by new Leader launch plans.
//! Native argv adaptation is pure; no Worker path reads this configuration.

use crate::provider::{wire::provider_wire, Provider};
use serde::Serialize;
use serde_json::{json, Value};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct PromptError {
    pub reason: &'static str,
    pub error: String,
    pub config_path: Option<PathBuf>,
    pub io_path: Option<PathBuf>,
    pub io_kind: Option<String>,
    pub provider: Option<&'static str>,
    pub carrier: Option<&'static str>,
    pub exit_code: Option<i32>,
    pub action: &'static str,
}

impl PromptError {
    pub(crate) fn new(reason: &'static str, error: &str, path: Option<&Path>) -> Self {
        Self {
            reason, error: error.to_string(), config_path: path.map(Path::to_path_buf),
            io_path: None, io_kind: None, provider: None, carrier: None, exit_code: None,
            action: "Inspect the reported path; use team-agent leader-prompt --help to set, append or clear the text",
        }
    }
    pub(crate) fn io(reason: &'static str, path: &Path, error: io::Error) -> Self {
        let mut result = Self::new(reason, "Leader prompt I/O failed", Some(path));
        result.io_path = Some(path.to_path_buf());
        result.io_kind = Some(format!("{:?}", error.kind()));
        result
    }
    pub(crate) fn at_config(mut self, path: &Path) -> Self {
        self.config_path = Some(path.to_path_buf());
        self
    }
    pub(crate) fn for_provider(mut self, provider: Provider) -> Self {
        self.provider = Some(provider_wire(provider));
        self.carrier = Some(carrier(provider));
        self
    }
    pub(crate) fn report(&self) -> Value {
        json!({"ok": false, "reason": self.reason, "error": self.error,
            "config_path": self.config_path, "io_path": self.io_path,
            "io_kind": self.io_kind, "provider": self.provider, "carrier": self.carrier,
            "exit_code": self.exit_code, "action": self.action})
    }
}
impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}; provider={:?}; carrier={:?}; config={:?}; io_path={:?}; io_kind={:?}; exit_code={:?}; action: {}",
            self.reason, self.error, self.provider, self.carrier, self.config_path, self.io_path, self.io_kind, self.exit_code, self.action)
    }
}
impl std::error::Error for PromptError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptMetadata {
    pub config_path: PathBuf,
    pub carrier: &'static str,
    pub bytes: usize,
}

pub(crate) struct StoredPrompt {
    pub path: PathBuf,
    pub text: String,
}
impl StoredPrompt {
    pub(crate) fn metadata(&self, provider: Provider) -> PromptMetadata {
        PromptMetadata {
            config_path: self.path.clone(),
            bytes: self.text.len(),
            carrier: carrier(provider),
        }
    }
}

fn carrier(provider: Provider) -> &'static str {
    match provider {
        Provider::Pi | Provider::Claude | Provider::ClaudeCode => "--append-system-prompt",
        Provider::Codex => "developer_instructions",
        Provider::Grok => "--rules",
        _ => "unsupported",
    }
}

fn path_from_home(home: Option<std::ffi::OsString>) -> Result<PathBuf, PromptError> {
    home.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(".team-agent/leader-prompt.txt"))
        .ok_or_else(|| {
            PromptError::new(
                "leader_prompt_home_unavailable",
                "An absolute host HOME is required",
                None,
            )
        })
}
pub(crate) fn config_path() -> Result<PathBuf, PromptError> {
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = home
        .filter(|raw| Path::new(raw).is_absolute())
        .or_else(|| std::env::var_os("USERPROFILE"));
    path_from_home(home)
}

fn regular_target(path: &Path, reason: &'static str) -> Result<bool, PromptError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(PromptError::new(
            reason,
            "Expected a regular file, not a symlink or special file",
            Some(path),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PromptError::io(reason, path, error)),
    }
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}
pub(crate) fn validate_input(text: &str, path: &Path) -> Result<(), PromptError> {
    if text.is_empty() || text.contains('\0') {
        return Err(PromptError::new(
            "leader_prompt_invalid_input",
            "Text must be nonempty UTF-8 without NUL; use clear to remove it",
            Some(path),
        ));
    }
    Ok(())
}
pub(crate) fn read(path: &Path) -> Result<Option<String>, PromptError> {
    if !regular_target(path, "leader_prompt_unreadable")? {
        return Ok(None);
    }
    let mut file = match private_options().read(true).open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && !regular_target(path, "leader_prompt_unreadable")? =>
        {
            return Ok(None)
        }
        Err(error) => return Err(PromptError::io("leader_prompt_unreadable", path, error)),
    };
    if !file
        .metadata()
        .map_err(|error| PromptError::io("leader_prompt_unreadable", path, error))?
        .is_file()
    {
        return Err(PromptError::new(
            "leader_prompt_unreadable",
            "Expected a regular prompt file",
            Some(path),
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| PromptError::io("leader_prompt_unreadable", path, error))?;
    if text.contains('\0') {
        return Err(PromptError::new(
            "leader_prompt_unreadable",
            "Prompt file contains NUL; set or clear it",
            Some(path),
        ));
    }
    Ok((!text.is_empty()).then_some(text))
}
/// Missing HOME retains historical launch behavior. A determined, bad file does not.
pub(crate) fn load() -> Result<Option<StoredPrompt>, PromptError> {
    let path = match config_path() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    read(&path).map(|text| text.map(|text| StoredPrompt { path, text }))
}

pub(crate) enum Mutation {
    Set(String),
    Append(String),
    Clear,
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = crate::platform::file_lock::unlock(&self.0);
    }
}
fn acquire_lock(path: &Path) -> Result<Lock, PromptError> {
    let parent = path.parent().ok_or_else(|| {
        PromptError::new(
            "leader_prompt_write_failed",
            "Configuration has no parent directory",
            Some(path),
        )
    })?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && parent.is_dir() => {}
        Err(error) => {
            return Err(PromptError::io("leader_prompt_write_failed", parent, error).at_config(path))
        }
    }
    let lock_path = parent.join("leader-prompt.lock");
    regular_target(&lock_path, "leader_prompt_write_failed")
        .map_err(|error| error.at_config(path))?;
    let file = private_options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            PromptError::io("leader_prompt_write_failed", &lock_path, error).at_config(path)
        })?;
    if !file
        .metadata()
        .map_err(|error| {
            PromptError::io("leader_prompt_write_failed", &lock_path, error).at_config(path)
        })?
        .is_file()
    {
        return Err(PromptError::new(
            "leader_prompt_write_failed",
            "Expected a regular lock file",
            Some(path),
        ));
    }
    let started = Instant::now();
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => return Ok(Lock(file)),
            Ok(false) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Ok(false) => {
                return Err(PromptError::new(
                    "leader_prompt_lock_timeout",
                    "Leader prompt write lock timed out",
                    Some(path),
                ))
            }
            Err(error) => {
                return Err(
                    PromptError::io("leader_prompt_write_failed", &lock_path, error)
                        .at_config(path),
                )
            }
        }
    }
}
fn write_atomic(path: &Path, text: &str) -> Result<(), PromptError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_file_name(format!(
        ".leader-prompt-{}-{nanos}-{id}.tmp",
        std::process::id()
    ));
    let mut file = private_options()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| {
            PromptError::io("leader_prompt_write_failed", &temp, error).at_config(path)
        })?;
    let result = (|| {
        file.write_all(text.as_bytes())
            .and_then(|_| file.flush())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                PromptError::io("leader_prompt_write_failed", &temp, error).at_config(path)
            })?;
        drop(file);
        // Rename replaces this directory entry; it never follows a target symlink.
        regular_target(path, "leader_prompt_write_failed")?;
        fs::rename(&temp, path)
            .map_err(|error| PromptError::io("leader_prompt_write_failed", path, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
pub(crate) fn concatenate(old: &str, input: &str) -> String {
    if old.is_empty() {
        input.to_string()
    } else {
        format!("{old}\n\n{input}")
    }
}
pub(crate) fn mutate(path: &Path, mutation: Mutation) -> Result<bool, PromptError> {
    if let Mutation::Set(text) | Mutation::Append(text) = &mutation {
        validate_input(text, path)?;
    }
    let present = regular_target(path, "leader_prompt_write_failed")?;
    if !present && matches!(mutation, Mutation::Clear) {
        return Ok(false);
    }
    // A bad append source does not create lock apparatus; set/clear can repair bytes.
    if matches!(mutation, Mutation::Append(_)) {
        read(path)?;
    }
    let _lock = acquire_lock(path)?;
    regular_target(path, "leader_prompt_write_failed")?;
    match mutation {
        Mutation::Set(text) => write_atomic(path, &text)?,
        Mutation::Append(text) => write_atomic(
            path,
            &concatenate(read(path)?.as_deref().unwrap_or(""), &text),
        )?,
        Mutation::Clear => match fs::remove_file(path) {
            Ok(()) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(PromptError::io("leader_prompt_write_failed", path, error)),
        },
    }
    Ok(true)
}

fn conflict(provider: Provider, path: &Path) -> PromptError {
    let mut error = PromptError::new(
        "leader_prompt_conflict",
        "Ambiguous or unsupported native supplemental prompt argument",
        Some(path),
    )
    .for_provider(provider);
    error.action = "Move the supplemental text into team-agent leader-prompt and remove conflicting native arguments";
    error
}
/// Returns the value token and whether the flag/value share that token.
fn inline_slot(
    argv: &[String],
    flag: &str,
    provider: Provider,
    path: &Path,
) -> Result<Option<(usize, bool)>, PromptError> {
    let end = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len());
    let mut slot = None;
    let mut i = 1;
    while i < end {
        let token = &argv[i];
        // Claude's core prompt fields own the following literal token. It is
        // not a sibling flag even when the text begins with our append flag.
        if matches!(provider, Provider::Claude | Provider::ClaudeCode)
            && matches!(token.as_str(), "--system-prompt" | "--system-prompt-file")
        {
            if i + 1 >= end {
                return Err(conflict(provider, path));
            }
            i += 2;
            continue;
        }
        let candidate = if token == flag {
            if i + 1 >= end || argv[i + 1].starts_with(flag) {
                return Err(conflict(provider, path));
            }
            i += 1;
            Some((i, false))
        } else if token.starts_with(&format!("{flag}=")) {
            Some((i, true))
        } else if token.starts_with(&format!("{flag}-")) {
            return Err(conflict(provider, path));
        } else {
            None
        };
        if let Some(candidate) = candidate {
            if slot.replace(candidate).is_some() {
                return Err(conflict(provider, path));
            }
        }
        i += 1;
    }
    Ok(slot)
}
/// Validate routed Pi options before materializing any MCP files. The existing
/// Leader contract stays first, so arbitrary supplemental text is not a file carrier.
pub(crate) fn compose_pi_contract(
    contract: &str,
    routed: &[String],
    text: &str,
    path: &Path,
) -> Result<String, PromptError> {
    let mut preview = routed.to_vec();
    preview.extend(["--append-system-prompt".to_string(), contract.to_string()]);
    let index = preview.len() - 1;
    if inline_slot(&preview, "--append-system-prompt", Provider::Pi, path)? != Some((index, false))
    {
        return Err(conflict(Provider::Pi, path));
    }
    inject(Provider::Pi, &mut preview, text, path)?;
    Ok(preview[index].clone())
}
pub(crate) fn inject(
    provider: Provider,
    argv: &mut Vec<String>,
    text: &str,
    path: &Path,
) -> Result<(), PromptError> {
    validate_input(text, path).map_err(|error| error.for_provider(provider))?;
    if provider == Provider::Codex {
        return inject_codex(argv, text, path);
    }
    let flag = match provider {
        Provider::Pi | Provider::Claude | Provider::ClaudeCode => "--append-system-prompt",
        Provider::Grok => "--rules",
        _ => {
            let mut error = PromptError::new(
                "leader_prompt_unsupported",
                "This Leader provider has no admitted process-local append carrier",
                Some(path),
            )
            .for_provider(provider);
            error.action = "Use pi, claude, codex or grok, or clear leader-prompt; Cursor requires native private-plugin isolation evidence";
            return Err(error);
        }
    };
    if let Some((index, joined)) = inline_slot(argv, flag, provider, path)? {
        let old = if joined {
            &argv[index][flag.len() + 1..]
        } else {
            &argv[index]
        };
        let combined = concatenate(old, text);
        argv[index] = if joined {
            format!("{flag}={combined}")
        } else {
            combined
        };
    } else if argv.is_empty() {
        return Err(conflict(provider, path));
    } else {
        argv.splice(1..1, [flag.to_string(), text.to_string()]);
    }
    Ok(())
}
fn inject_codex(argv: &mut Vec<String>, text: &str, path: &Path) -> Result<(), PromptError> {
    let provider = Provider::Codex;
    let end = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len());
    let mut slot = None;
    let mut i = 1;
    while i < end {
        let token = &argv[i];
        let (index, value, joined) = if matches!(token.as_str(), "-c" | "--config") {
            if i + 1 >= end {
                return Err(conflict(provider, path));
            }
            i += 1;
            (i, argv[i].as_str(), false)
        } else if let Some(value) = token.strip_prefix("--config=") {
            (i, value, true)
        } else if token.strip_prefix("-c").is_some_and(|value| {
            let key = value
                .trim_start_matches('=')
                .split('=')
                .next()
                .unwrap_or("")
                .trim();
            matches!(
                key,
                "developer_instructions"
                    | "\"developer_instructions\""
                    | "'developer_instructions'"
            )
        }) {
            return Err(conflict(provider, path));
        } else {
            i += 1;
            continue;
        };
        let (key, rhs) = value.split_once('=').unwrap_or((value, ""));
        if matches!(
            key.trim(),
            "\"developer_instructions\"" | "'developer_instructions'"
        ) {
            return Err(conflict(provider, path));
        }
        if key.trim() == "developer_instructions" {
            let old: String = serde_json::from_str(rhs).map_err(|_| conflict(provider, path))?;
            if old.contains('\0') || slot.replace((index, joined, old)).is_some() {
                return Err(conflict(provider, path));
            }
        }
        i += 1;
    }
    let (old, index) = match slot {
        Some((index, joined, old)) => (old, Some((index, joined))),
        None => (String::new(), None),
    };
    let combined = concatenate(&old, text);
    let encoded = serde_json::to_string(&combined).map_err(|_| conflict(provider, path))?;
    let value = format!("developer_instructions={encoded}");
    if let Some((index, joined)) = index {
        argv[index] = if joined {
            format!("--config={value}")
        } else {
            value
        };
    } else if argv.is_empty() {
        return Err(conflict(provider, path));
    } else {
        argv.splice(1..1, ["-c".to_string(), value]);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static ID: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ta-leader-prompt-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join(".team-agent/leader-prompt.txt")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn leader_prompt_storage_bytes_clear_and_repair() {
        let f = Fixture::new();
        let path = f.path();
        assert!(!mutate(&path, Mutation::Clear).unwrap());
        assert!(!path.parent().unwrap().exists());
        let text = "  rules🙂\r\nquote\"\\\t--help\n";
        mutate(&path, Mutation::Set(text.into())).unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some(text));
        mutate(&path, Mutation::Append(" \n ".into())).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            format!("{text}\n\n \n ").as_bytes()
        );
        let old = fs::read(&path).unwrap();
        for input in ["", "bad\0text"] {
            assert!(mutate(&path, Mutation::Set(input.into())).is_err());
            assert_eq!(fs::read(&path).unwrap(), old);
        }
        fs::write(&path, [0xff]).unwrap();
        assert_eq!(read(&path).unwrap_err().reason, "leader_prompt_unreadable");
        assert!(mutate(&path, Mutation::Append("new".into())).is_err());
        assert_eq!(fs::read(&path).unwrap(), [0xff]);
        mutate(&path, Mutation::Set("repaired".into())).unwrap();
        fs::write(&path, b"nul\0").unwrap();
        assert!(read(&path).is_err());
        mutate(&path, Mutation::Clear).unwrap();
        fs::write(&path, []).unwrap();
        assert!(read(&path).unwrap().is_none());
    }
    #[test]
    fn leader_prompt_concurrent_appends_do_not_lose_updates() {
        let f = Fixture::new();
        let path = f.path();
        let threads: Vec<_> = (0..12)
            .map(|id| {
                let path = path.clone();
                std::thread::spawn(move || {
                    mutate(&path, Mutation::Append(format!("entry-{id}"))).unwrap()
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let text = read(&path).unwrap().unwrap();
        let entries: std::collections::BTreeSet<String> =
            text.split("\n\n").map(str::to_string).collect();
        assert_eq!(entries, (0..12).map(|id| format!("entry-{id}")).collect());
        assert_eq!(text.split("\n\n").count(), 12);
    }
    #[test]
    fn leader_prompt_temp_io_error_identifies_the_actual_operation_path() {
        let f = Fixture::new();
        let path = f.0.join("missing-parent/leader-prompt.txt");
        let error = write_atomic(&path, "new").unwrap_err();
        assert_eq!(error.config_path.as_deref(), Some(path.as_path()));
        assert_eq!(error.io_kind.as_deref(), Some("NotFound"));
        let actual = error.io_path.unwrap();
        assert_eq!(actual.parent(), path.parent());
        assert!(actual
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".leader-prompt-"));
        assert_ne!(actual, path);
    }
    #[test]
    fn leader_prompt_bounded_lock_timeout_preserves_body_and_foreign_temp() {
        let f = Fixture::new();
        let path = f.path();
        mutate(&path, Mutation::Set("old".into())).unwrap();
        let foreign = path.parent().unwrap().join(".leader-prompt-foreign.tmp");
        fs::write(&foreign, "foreign").unwrap();
        let _held = acquire_lock(&path).unwrap();
        let started = Instant::now();
        assert_eq!(
            mutate(&path, Mutation::Append("new".into()))
                .unwrap_err()
                .reason,
            "leader_prompt_lock_timeout"
        );
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(fs::read_to_string(path).unwrap(), "old");
        assert_eq!(fs::read_to_string(foreign).unwrap(), "foreign");
    }
    #[cfg(unix)]
    #[test]
    fn leader_prompt_private_modes_and_symlink_refusal() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let f = Fixture::new();
        let path = f.path();
        mutate(&path, Mutation::Set("private".into())).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let target = f.0.join("foreign");
        fs::write(&target, "unchanged").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert!(read(&path).is_err());
        for mutation in [
            Mutation::Set("new".into()),
            Mutation::Append("new".into()),
            Mutation::Clear,
        ] {
            assert!(mutate(&path, mutation).is_err());
            assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        }
        fs::remove_file(&path).unwrap();
        let lock = path.parent().unwrap().join("leader-prompt.lock");
        fs::remove_file(&lock).unwrap();
        symlink(&target, &lock).unwrap();
        assert!(mutate(&path, Mutation::Set("new".into())).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged");
        fs::remove_file(&lock).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        mutate(&path, Mutation::Set("new".into())).unwrap();
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    #[test]
    fn leader_prompt_home_requires_absolute_path() {
        for home in [None, Some("".into()), Some("relative".into())] {
            assert!(path_from_home(home).is_err());
        }
        assert!(path_from_home(Some(std::env::temp_dir().into_os_string()))
            .unwrap()
            .is_absolute());
    }
    #[test]
    fn leader_prompt_inline_append_preserves_tokens_and_delimiter_data() {
        let path = Path::new("/home/example/.team-agent/leader-prompt.txt");
        for (provider, flag) in [
            (Provider::Pi, "--append-system-prompt"),
            (Provider::Claude, "--append-system-prompt"),
            (Provider::Grok, "--rules"),
        ] {
            let mut argv = args(&[
                "native", "--model", "model", flag, "old", "--", flag, "literal",
            ]);
            inject(provider, &mut argv, "new🙂\n", path).unwrap();
            assert_eq!(
                argv,
                args(&[
                    "native",
                    "--model",
                    "model",
                    flag,
                    "old\n\nnew🙂\n",
                    "--",
                    flag,
                    "literal"
                ])
            );
            let mut joined = args(&["native", &format!("{flag}=old"), "resume"]);
            inject(provider, &mut joined, "new", path).unwrap();
            assert_eq!(
                joined,
                args(&["native", &format!("{flag}=old\n\nnew"), "resume"])
            );
            for tail in [
                args(&[flag]),
                args(&[flag, flag, "two"]),
                args(&[flag, "one", flag, "two"]),
                args(&[&format!("{flag}-file"), "file"]),
            ] {
                let mut bad = args(&["native"]);
                bad.extend(tail);
                let before = bad.clone();
                assert_eq!(
                    inject(provider, &mut bad, "new", path).unwrap_err().reason,
                    "leader_prompt_conflict"
                );
                assert_eq!(bad, before);
            }
        }
    }
    #[test]
    fn leader_prompt_pi_preserves_contract_and_refuses_route_override() {
        let path = Path::new("/home/example/.team-agent/leader-prompt.txt");
        let contract = "Team Agent Leader contract\nKeep team-local tools.";
        assert_eq!(
            compose_pi_contract(
                contract,
                &args(&["pi", "--verbose"]),
                "/looks/like/file",
                path
            )
            .unwrap(),
            format!("{contract}\n\n/looks/like/file")
        );
        for routed in [
            args(&["pi", "--append-system-prompt", "other"]),
            args(&["pi", "--append-system-prompt-file", "file"]),
            args(&["pi", "--", "literal"]),
        ] {
            assert_eq!(
                compose_pi_contract(contract, &routed, "supplement", path)
                    .unwrap_err()
                    .reason,
                "leader_prompt_conflict"
            );
        }
    }
    #[test]
    fn leader_prompt_claude_core_prompt_values_are_not_supplemental_sibling_flags() {
        let path = Path::new("/home/example/.team-agent/leader-prompt.txt");
        for provider in [Provider::Claude, Provider::ClaudeCode] {
            for flag in ["--system-prompt", "--system-prompt-file"] {
                for literal in [
                    "--append-system-prompt",
                    "--append-system-prompt=literal",
                    "--append-system-prompt-file",
                ] {
                    let original = args(&["claude", flag, literal, "Initial task"]);
                    let mut argv = original.clone();
                    inject(provider, &mut argv, "GLOBAL_309", path).unwrap();
                    assert_eq!(
                        &argv[..3],
                        args(&["claude", "--append-system-prompt", "GLOBAL_309"])
                    );
                    assert_eq!(&argv[3..], &original[1..]);
                }
            }
        }
    }
    #[test]
    fn leader_prompt_codex_owns_one_slot_and_preserves_resume() {
        let path = Path::new("/home/example/.team-agent/leader-prompt.txt");
        let text = "quote\"\\\r\n\t🙂";
        for (flag, joined) in [("-c", false), ("--config", false), ("--config", true)] {
            let value = "developer_instructions=\"old\"";
            let mut argv = if joined {
                args(&[
                    "codex",
                    &format!("{flag}={value}"),
                    "resume",
                    "--last",
                    "--",
                    "literal",
                ])
            } else {
                args(&["codex", flag, value, "resume", "--last", "--", "literal"])
            };
            let tail = argv[if joined { 2 } else { 3 }..].to_vec();
            inject(Provider::Codex, &mut argv, text, path).unwrap();
            let encoded = argv[if joined { 1 } else { 2 }]
                .split_once("developer_instructions=")
                .unwrap()
                .1;
            assert_eq!(
                serde_json::from_str::<String>(encoded).unwrap(),
                format!("old\n\n{text}")
            );
            assert_eq!(&argv[if joined { 2 } else { 3 }..], tail);
        }
        let mut fresh = args(&[
            "codex",
            "resume",
            "--last",
            "--",
            "developer_instructions='literal'",
        ]);
        inject(Provider::Codex, &mut fresh, text, path).unwrap();
        assert_eq!(
            &fresh[3..],
            args(&["resume", "--last", "--", "developer_instructions='literal'"])
        );
        for tokens in [
            args(&["codex", "-c", "developer_instructions='raw'"]),
            args(&[
                "codex",
                "-c",
                "developer_instructions=\"one\"",
                "--config=developer_instructions=\"two\"",
            ]),
            args(&["codex", "-cdeveloper_instructions=\"attached\""]),
            args(&["codex", "-c=developer_instructions=\"attached\""]),
            args(&[
                "codex",
                "--config",
                "\"developer_instructions\"=\"quoted key\"",
            ]),
            args(&[
                "codex",
                "--config",
                "'developer_instructions'=\"quoted key\"",
            ]),
        ] {
            let mut argv = tokens.clone();
            assert_eq!(
                inject(Provider::Codex, &mut argv, text, path)
                    .unwrap_err()
                    .reason,
                "leader_prompt_conflict"
            );
            assert_eq!(argv, tokens);
        }
    }
    #[test]
    fn leader_prompt_unadmitted_providers_fail_without_argv_mutation() {
        let path = Path::new("/home/example/.team-agent/leader-prompt.txt");
        for provider in [
            Provider::CursorAgent,
            Provider::Copilot,
            Provider::GeminiCli,
            Provider::Fake,
        ] {
            let mut argv = args(&["native", "--model", "model"]);
            let before = argv.clone();
            let error = inject(provider, &mut argv, "do not echo this sentinel", path).unwrap_err();
            assert_eq!(error.reason, "leader_prompt_unsupported");
            assert!(!error.to_string().contains("sentinel"));
            assert_eq!(argv, before);
        }
    }
}
