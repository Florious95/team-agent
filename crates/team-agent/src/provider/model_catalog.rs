//! Provider model discovery, bounded native catalog queries, and shared fuzzy search.
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::model::enums::Provider;
use crate::provider::wire::{parse_canonical_provider, provider_wire};

const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRecord {
    pub provider: String,
    pub vendor: String,
    pub id: String,
    pub display_name: String,
    pub default: Option<bool>,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    UnsupportedProvider,
    ExecutableUnavailable,
    Timeout,
    CommandFailed,
    OutputUnavailable,
    OutputReadFailed,
    OutputTooLarge,
    InputWriteFailed,
    InvalidUtf8,
    EmptyInput,
    Malformed,
    MalformedPiHeader,
    MalformedPiRow(usize),
    DuplicateIdentity(String),
    Empty,
    UnsupportedSchema,
    InvalidProtocol,
    TemporaryDirectoryUnavailable,
    CommandCouldNotBeObserved,
}

impl CatalogError {
    pub fn message(&self, provider: &str) -> String {
        let name = display_provider(provider);
        match self {
            Self::UnsupportedProvider => format!("unsupported model provider {provider:?}"),
            Self::ExecutableUnavailable => format!("{name} executable is unavailable on PATH"),
            Self::Timeout => format!("{name} model catalog command timed out"),
            Self::CommandFailed => format!("{name} model catalog command failed"),
            Self::OutputUnavailable => format!("{name} model catalog output unavailable"),
            Self::OutputReadFailed => format!("{name} model catalog could not be read"),
            Self::OutputTooLarge => format!("{name} model catalog exceeds the bounded output limit"),
            Self::InputWriteFailed => format!("{name} model catalog request could not be written"),
            Self::InvalidUtf8 => format!("{name} model catalog is not valid UTF-8"),
            Self::EmptyInput => "Pi model catalog is empty".into(),
            Self::Malformed => format!("{name} model catalog is malformed"),
            Self::MalformedPiHeader => "Pi model catalog has an unexpected header".into(),
            Self::MalformedPiRow(row) => format!("Pi model catalog row {row} is malformed"),
            Self::DuplicateIdentity(_) => format!("{name} model catalog contains duplicate model ids"),
            Self::Empty => format!("{name} model catalog is empty"),
            Self::UnsupportedSchema => format!("{name} model catalog uses an unsupported schema"),
            Self::InvalidProtocol => format!("{name} model catalog returned an invalid protocol response"),
            Self::TemporaryDirectoryUnavailable => "Claude model catalog could not create an isolated temporary directory".into(),
            Self::CommandCouldNotBeObserved => format!("{name} model catalog command could not be observed"),
        }
    }

    pub fn action(&self, provider: &str) -> String {
        let executable = match provider {
            "pi" => "pi",
            "codex" => "codex",
            "claude" | "claude_code" => "claude",
            "grok" => "grok",
            _ => "provider CLI",
        };
        match self {
            Self::UnsupportedProvider => "choose a supported provider: pi, codex, claude, claude_code, or grok".into(),
            Self::ExecutableUnavailable => format!("install or repair the PATH-first `{executable}` executable, then retry `team-agent models --provider {provider}`"),
            Self::UnsupportedSchema => format!("upgrade `{executable}` to a version that supports its model catalog protocol, then retry `team-agent models --provider {provider}`"),
            Self::CommandFailed => format!("run `{executable}` directly to check catalog/account access, then retry `team-agent models --provider {provider}`"),
            _ => format!("check the native `{executable}` catalog command, then retry `team-agent models --provider {provider}`"),
        }
    }

    pub(crate) fn pi_legacy_message(&self) -> String {
        match self {
            Self::ExecutableUnavailable => "Pi executable is unavailable on PATH".into(),
            Self::Timeout => "Pi model catalog command timed out".into(),
            Self::CommandFailed => "Pi model catalog command failed".into(),
            Self::OutputUnavailable => "Pi model catalog output unavailable".into(),
            Self::OutputReadFailed => "Pi model catalog could not be read".into(),
            Self::OutputTooLarge => "Pi model catalog exceeds the bounded output limit".into(),
            Self::InputWriteFailed => "Pi model catalog request could not be written".into(),
            Self::InvalidUtf8 => "Pi model catalog is not UTF-8".into(),
            Self::EmptyInput => "Pi model catalog is empty".into(),
            Self::MalformedPiHeader => "Pi model catalog has an unexpected header".into(),
            Self::MalformedPiRow(row) => format!("Pi model catalog row {row} is malformed"),
            Self::DuplicateIdentity(id) => format!("Pi model catalog contains duplicate exact id {id:?}"),
            Self::Empty => "Pi model catalog contains no models".into(),
            Self::CommandCouldNotBeObserved => "Pi model catalog command could not be observed".into(),
            _ => "Pi model catalog is invalid".into(),
        }
    }
}

fn display_provider(provider: &str) -> &str {
    match provider {
        "pi" => "Pi",
        "codex" => "Codex",
        "claude" | "claude_code" => "Claude",
        "grok" => "Grok",
        _ => provider,
    }
}

/// Only the documented catalog names are accepted; launch aliases stay unchanged.
pub fn parse_catalog_provider(name: &str) -> Option<Provider> {
    parse_canonical_provider(name).filter(|provider| catalog_source(*provider).is_some())
}

#[derive(Clone, Copy)]
enum CatalogSource {
    Codex,
    Grok,
}

// Exhaustive by design: a new Provider variant must make an explicit catalog decision.
// `team-agent models --provider claude|claude_code|pi` reads the contract
// leaf's catalog (contract_runtime::discovery) before reaching this table.
fn catalog_source(provider: Provider) -> Option<CatalogSource> {
    match provider {
        Provider::Codex => Some(CatalogSource::Codex),
        Provider::Grok => Some(CatalogSource::Grok),
        Provider::Claude
        | Provider::ClaudeCode
        | Provider::Pi
        | Provider::Copilot
        | Provider::GeminiCli
        | Provider::Fake => None,
    }
}

pub fn discover_model_catalog(provider_name: &str) -> Result<Vec<ModelRecord>, CatalogError> {
    let provider = parse_catalog_provider(provider_name).ok_or(CatalogError::UnsupportedProvider)?;
    let source = catalog_source(provider).ok_or(CatalogError::UnsupportedProvider)?;
    let requested_provider = provider_wire(provider);
    let records = match source {
        CatalogSource::Codex => {
            let bytes = run_native("codex", &["debug", "models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_codex_catalog(&bytes, requested_provider)?
        }
        CatalogSource::Grok => {
            let bytes = run_native("grok", &["models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_grok_catalog(&bytes)?
        }
    };
    Ok(records)
}

/// One shared token-AND/field-OR, case-insensitive substring matcher.
pub fn model_matches(record: &ModelRecord, query: &str) -> bool {
    let fields = std::iter::once(record.provider.as_str())
        .chain(std::iter::once(record.vendor.as_str()))
        .chain(std::iter::once(record.id.as_str()))
        .chain(std::iter::once(record.display_name.as_str()))
        .chain(record.aliases.iter().map(String::as_str))
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|token| fields.iter().any(|field| field.contains(&token)))
}

pub(crate) fn parse_pi_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, CatalogError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidUtf8)?;
    let mut lines = text.lines();
    let header = lines.next().ok_or(CatalogError::EmptyInput)?;
    let columns = header.split_whitespace().collect::<Vec<_>>();
    if columns.get(0) != Some(&"provider") || columns.get(1) != Some(&"model") {
        return Err(CatalogError::MalformedPiHeader);
    }
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let malformed = || CatalogError::MalformedPiRow(index + 2);
        let (vendor, rest) = line.trim().split_once(char::is_whitespace).ok_or_else(malformed)?;
        let mut id = rest.trim();
        for _ in 2..columns.len() {
            id = id.rsplit_once(char::is_whitespace).ok_or_else(malformed)?.0.trim_end();
        }
        if id.is_empty() {
            return Err(malformed());
        }
        let exact = format!("{vendor}/{id}");
        if !seen.insert(exact.clone()) {
            return Err(CatalogError::DuplicateIdentity(exact));
        }
        records.push(ModelRecord {
            provider: "pi".into(), vendor: vendor.into(), display_name: exact.clone(), id: exact,
            default: None, aliases: Vec::new(),
        });
    }
    if records.is_empty() { return Err(CatalogError::Empty); }
    Ok(records)
}

fn parse_grok_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, CatalogError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidUtf8)?;
    let mut has_auth_notice = false;
    let mut has_models_header = false;
    let mut declared_default: Option<String> = None;
    let mut starred_default: Option<String> = None;
    let mut marked_default: Option<String> = None;
    let mut seen = HashSet::new();
    let mut records = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !has_models_header {
            if matches!(line, "You are logged in with grok.com." | "You are not authenticated.") {
                if has_auth_notice {
                    return Err(CatalogError::Malformed);
                }
                has_auth_notice = true;
                continue;
            }
            if let Some(id) = line.strip_prefix("Default model: ") {
                if declared_default.is_some() || id.is_empty() {
                    return Err(CatalogError::Malformed);
                }
                validate_identity(id)?;
                declared_default = Some(id.to_string());
                continue;
            }
            if line == "Available models:" {
                has_models_header = true;
                continue;
            }
            return Err(CatalogError::Malformed);
        }

        let (is_starred, row) = if let Some(row) = line.strip_prefix("- ") {
            (false, row)
        } else if let Some(row) = line.strip_prefix("* ") {
            (true, row)
        } else {
            return Err(CatalogError::Malformed);
        };
        if row == "(default)" {
            return Err(CatalogError::Malformed);
        }
        let (id, has_default_marker) = row
            .strip_suffix(" (default)")
            .map_or((row, false), |id| (id, true));
        if id.is_empty() {
            return Err(CatalogError::Malformed);
        }
        validate_identity(id)?;
        if !seen.insert(id.to_string()) {
            return Err(CatalogError::DuplicateIdentity(id.to_string()));
        }
        if is_starred {
            if starred_default.as_deref().is_some_and(|current| current != id) {
                return Err(CatalogError::Malformed);
            }
            starred_default = Some(id.to_string());
        }
        if has_default_marker {
            if marked_default.as_deref().is_some_and(|current| current != id) {
                return Err(CatalogError::Malformed);
            }
            marked_default = Some(id.to_string());
        }
        records.push(ModelRecord {
            provider: "grok".into(),
            vendor: "xai".into(),
            id: id.into(),
            display_name: id.into(),
            default: None,
            aliases: Vec::new(),
        });
    }
    if !has_models_header {
        return Err(CatalogError::Malformed);
    }
    if records.is_empty() {
        return Err(CatalogError::Empty);
    }

    let mut default_id: Option<&str> = None;
    for candidate in [
        declared_default.as_deref(),
        starred_default.as_deref(),
        marked_default.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if default_id.is_some_and(|current| current != candidate) {
            return Err(CatalogError::Malformed);
        }
        default_id = Some(candidate);
    }
    if let Some(default_id) = default_id {
        if !seen.contains(default_id) {
            return Err(CatalogError::Malformed);
        }
        for record in &mut records {
            record.default = Some(record.id.as_str() == default_id);
        }
    }
    Ok(records)
}

fn parse_codex_catalog(bytes: &[u8], provider: &str) -> Result<Vec<ModelRecord>, CatalogError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| CatalogError::Malformed)?;
    let rows = value.get("models").and_then(serde_json::Value::as_array).ok_or(CatalogError::UnsupportedSchema)?;
    if rows.is_empty() { return Err(CatalogError::Empty); }
    let mut seen = HashSet::new();
    let mut visible = Vec::new();
    for row in rows {
        let row = row.as_object().ok_or(CatalogError::Malformed)?;
        let slug = required_string(row.get("slug"))?;
        let display = required_string(row.get("display_name"))?;
        let visibility = required_string(row.get("visibility"))?;
        validate_identity(slug)?;
        validate_display(display)?;
        if !seen.insert(slug.to_string()) { return Err(CatalogError::DuplicateIdentity(slug.into())); }
        match visibility {
            "list" => visible.push(ModelRecord { provider: provider.into(), vendor: "openai".into(), id: slug.into(), display_name: display.into(), default: None, aliases: Vec::new() }),
            "hide" | "none" => {},
            _ => return Err(CatalogError::UnsupportedSchema),
        }
    }
    if visible.is_empty() { return Err(CatalogError::Empty); }
    Ok(visible)
}

fn required_string(value: Option<&serde_json::Value>) -> Result<&str, CatalogError> {
    let value = value.and_then(serde_json::Value::as_str).ok_or(CatalogError::UnsupportedSchema)?;
    if value.trim().is_empty() { return Err(CatalogError::UnsupportedSchema); }
    Ok(value)
}

fn validate_identity(value: &str) -> Result<(), CatalogError> {
    if value.chars().any(|ch| ch.is_whitespace() || ch.is_control()) { Err(CatalogError::Malformed) } else { Ok(()) }
}

fn validate_display(value: &str) -> Result<(), CatalogError> {
    if value.chars().any(char::is_control) { Err(CatalogError::Malformed) } else { Ok(()) }
}

pub(crate) trait CatalogObserver {
    fn spawned(&mut self, _pid: u32) {}
    fn exited(&mut self, _status: &ExitStatus) {}
    fn output_timeout(&mut self) {}
}

pub(crate) struct NoopObserver;
impl CatalogObserver for NoopObserver {}

fn run_native(
    program: &str,
    args: &[&str],
    input: Option<&[u8]>,
    cwd: Option<&Path>,
    remove_env: &[&str],
    timeout: Duration,
    max_bytes: u64,
    observer: &mut dyn CatalogObserver,
) -> Result<Vec<u8>, CatalogError> {
    run_command(Path::new(program), args, input, cwd, remove_env, &[], timeout, max_bytes, observer)
}

pub(crate) fn run_command(
    program: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    cwd: Option<&Path>,
    remove_env: &[&str],
    extra_env: &[(OsString, OsString)],
    timeout: Duration,
    max_bytes: u64,
    observer: &mut dyn CatalogObserver,
) -> Result<Vec<u8>, CatalogError> {
    let started = Instant::now();
    let mut command = Command::new(program);
    command.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(cwd) = cwd { command.current_dir(cwd); }
    for name in remove_env { command.env_remove(name); }
    command.envs(extra_env.iter().cloned());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|_| CatalogError::ExecutableUnavailable)?;
    let pid = child.id();
    observer.spawned(pid);
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => { terminate(&mut child, pid); return Err(CatalogError::OutputUnavailable); }
    };
    let (reader_tx, reader_rx) = mpsc::channel();
    let reader = match std::thread::Builder::new().name("model-catalog-reader".into()).spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes);
        let _ = reader_tx.send(result.map(|_| bytes));
    }) {
        Ok(reader) => reader,
        Err(_) => { terminate(&mut child, pid); return Err(CatalogError::OutputReadFailed); }
    };
    let writer = if let Some(input) = input {
        let Some(mut stdin) = child.stdin.take() else { terminate(&mut child, pid); return Err(CatalogError::InputWriteFailed); };
        let input = input.to_vec();
        let (tx, rx) = mpsc::channel();
        let writer = match std::thread::Builder::new().name("model-catalog-writer".into()).spawn(move || {
            let result = stdin.write_all(&input);
            drop(stdin);
            let _ = tx.send(result.is_ok());
        }) {
            Ok(writer) => writer,
            Err(_) => { terminate(&mut child, pid); return Err(CatalogError::InputWriteFailed); }
        };
        Some((writer, rx))
    } else { None };
    let mut output = None;
    let mut pending_output = None;
    let mut output_ready_at: Option<Instant> = None;
    let mut too_large_at = None;
    let status = loop {
        match pending_output.take().map_or_else(|| reader_rx.try_recv(), Ok) {
            Ok(Ok(bytes)) => {
                if bytes.len() as u64 > max_bytes {
                    too_large_at.get_or_insert_with(Instant::now);
                } else {
                    output = Some(bytes);
                }
                output_ready_at = Some(Instant::now());
            }
            Ok(Err(_)) => { terminate(&mut child, pid); return Err(CatalogError::OutputReadFailed); }
            Err(mpsc::TryRecvError::Disconnected) if output.is_none() && too_large_at.is_none() => { terminate(&mut child, pid); return Err(CatalogError::OutputReadFailed); }
            Err(mpsc::TryRecvError::Disconnected) | Err(mpsc::TryRecvError::Empty) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                observer.exited(&status);
                if !status.success() { terminate(&mut child, pid); return Err(CatalogError::CommandFailed); }
                if too_large_at.is_some() { terminate(&mut child, pid); return Err(CatalogError::OutputTooLarge); }
                break status;
            }
            Ok(None) if too_large_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(100)) => {
                terminate(&mut child, pid);
                return Err(CatalogError::OutputTooLarge);
            }
            Ok(None) if started.elapsed() < timeout => {
                let mut wait = timeout.saturating_sub(started.elapsed()).min(Duration::from_millis(10));
                if let Some(over_at) = too_large_at {
                    wait = wait.min(Duration::from_millis(100).saturating_sub(over_at.elapsed()));
                }
                if output.is_none() && too_large_at.is_none() {
                    // Reader completion wakes us instead of trailing a blind 10ms sleep.
                    if let Ok(result) = reader_rx.recv_timeout(wait) {
                        pending_output = Some(result);
                    }
                } else {
                    // stdout can close before exit; never busy-poll a disconnected channel.
                    let interval = output_ready_at.map_or(Duration::from_millis(10), |at| {
                        catalog_exit_poll_interval(at.elapsed())
                    });
                    std::thread::sleep(wait.min(interval));
                }
            }
            Ok(None) if too_large_at.is_some() => { terminate(&mut child, pid); return Err(CatalogError::OutputTooLarge); }
            Ok(None) => { terminate(&mut child, pid); return Err(CatalogError::Timeout); }
            Err(_) => { terminate(&mut child, pid); return Err(CatalogError::CommandCouldNotBeObserved); }
        }
    };
    let _ = status;
    if let Some((writer, result)) = writer {
        let remaining = timeout.saturating_sub(started.elapsed());
        match result.recv_timeout(remaining) {
            Ok(true) => { let _ = writer.join(); }
            Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => { terminate(&mut child, pid); return Err(CatalogError::InputWriteFailed); }
            Err(mpsc::RecvTimeoutError::Timeout) => { terminate(&mut child, pid); return Err(CatalogError::Timeout); }
        }
    }
    if output.is_none() {
        let remaining = timeout.saturating_sub(started.elapsed());
        match reader_rx.recv_timeout(remaining) {
            Ok(Ok(bytes)) if bytes.len() as u64 <= max_bytes => output = Some(bytes),
            Ok(Ok(_)) => { terminate(&mut child, pid); return Err(CatalogError::OutputTooLarge); }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Err(CatalogError::OutputReadFailed),
            Err(mpsc::RecvTimeoutError::Timeout) => { observer.output_timeout(); terminate(&mut child, pid); return Err(CatalogError::Timeout); }
        }
    }
    let _ = reader.join();
    output.ok_or(CatalogError::OutputReadFailed)
}

fn catalog_exit_poll_interval(elapsed: Duration) -> Duration {
    [1, 3, 6, 10]
        .into_iter()
        .map(Duration::from_millis)
        .find(|checkpoint| *checkpoint > elapsed)
        .map_or(Duration::from_millis(10), |checkpoint| {
            checkpoint.saturating_sub(elapsed)
        })
}

fn terminate(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    unsafe {
        // The process group was created for this one native catalog invocation.
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod command_wait_regressions {
    use super::*;

    #[derive(Default)]
    struct Observer {
        pid: Option<u32>,
        exits: Vec<Option<i32>>,
        output_timeouts: usize,
    }

    impl CatalogObserver for Observer {
        fn spawned(&mut self, pid: u32) { self.pid = Some(pid); }
        fn exited(&mut self, status: &ExitStatus) { self.exits.push(status.code()); }
        fn output_timeout(&mut self) { self.output_timeouts += 1; }
    }

    fn run(
        script: &str,
        input: Option<&[u8]>,
        timeout: Duration,
        max_bytes: u64,
        observer: &mut Observer,
    ) -> Result<Vec<u8>, CatalogError> {
        run_command(
            Path::new("/bin/sh"), &["-c", script], input, None, &[], &[],
            timeout, max_bytes, observer,
        )
    }

    fn assert_owned_child_gone(observer: &Observer) {
        let pid = observer.pid.unwrap();
        assert!(pid > 1);
        let status = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "owned child must be reaped");
    }

    #[test]
    fn eof_and_success_preserve_input_and_raw_output() {
        let mut observer = Observer::default();
        let input = b"catalog\0\xff\n";
        let output = run(
            "exec /bin/cat", Some(input), CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut observer,
        ).unwrap();
        assert_eq!(output, input.to_vec());
        assert_eq!(observer.exits, vec![Some(0)]);
        assert_eq!(observer.output_timeouts, 0);
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn stdout_eof_does_not_hide_a_later_nonzero_exit() {
        let mut observer = Observer::default();
        let error = run(
            "printf 'partial'; exec 1>&-; /bin/sleep 0.03; exit 17",
            None, CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut observer,
        ).unwrap_err();
        assert_eq!(error, CatalogError::CommandFailed);
        assert_eq!(observer.exits, vec![Some(17)]);
        assert_eq!(observer.output_timeouts, 0);
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn stdout_eof_does_not_disable_the_ten_second_process_timeout() {
        let mut observer = Observer::default();
        let started = Instant::now();
        let error = run(
            "exec 1>&-; exec /bin/sleep 30", None, CATALOG_TIMEOUT,
            MAX_CATALOG_BYTES, &mut observer,
        ).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(CATALOG_TIMEOUT, Duration::from_secs(10));
        assert_eq!(error, CatalogError::Timeout);
        assert!(elapsed >= CATALOG_TIMEOUT);
        assert!(elapsed < CATALOG_TIMEOUT + Duration::from_secs(2));
        assert!(observer.exits.is_empty());
        assert_eq!(observer.output_timeouts, 0);
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn exact_one_mib_is_allowed_and_one_more_byte_is_rejected() {
        assert_eq!(MAX_CATALOG_BYTES, 1024 * 1024);
        let mut observer = Observer::default();
        let script = format!("exec /usr/bin/head -c {MAX_CATALOG_BYTES} /dev/zero");
        let output = run(
            &script, None, CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut observer,
        ).unwrap();
        assert_eq!(output.len() as u64, MAX_CATALOG_BYTES);
        assert_owned_child_gone(&observer);
        let mut observer = Observer::default();
        let script = format!("exec /usr/bin/head -c {} /dev/zero", MAX_CATALOG_BYTES + 1);
        assert_eq!(
            run(&script, None, CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut observer),
            Err(CatalogError::OutputTooLarge)
        );
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn oversized_output_keeps_one_hundred_millisecond_exit_grace() {
        let mut observer = Observer::default();
        let started = Instant::now();
        let error = run(
            "printf '%65s' x; exec /bin/sleep 30", None, CATALOG_TIMEOUT, 64, &mut observer,
        ).unwrap_err();
        assert_eq!(error, CatalogError::OutputTooLarge);
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn exited_child_with_inherited_stdout_keeps_output_timeout_observation() {
        let mut observer = Observer::default();
        let error = run(
            "/bin/sleep 30 & exit 0", None, Duration::from_millis(200),
            MAX_CATALOG_BYTES, &mut observer,
        ).unwrap_err();
        assert_eq!(error, CatalogError::Timeout);
        assert_eq!(observer.exits, vec![Some(0)]);
        assert_eq!(observer.output_timeouts, 1);
        assert_owned_child_gone(&observer);
    }

    #[test]
    fn early_eof_polling_keeps_ten_millisecond_anchor_and_then_old_cadence() {
        let mut elapsed = Duration::ZERO;
        for (sleep_ms, check_ms) in [(1, 1), (2, 3), (3, 6), (4, 10), (10, 20)] {
            let interval = catalog_exit_poll_interval(elapsed);
            assert_eq!(interval, Duration::from_millis(sleep_ms));
            elapsed += interval;
            assert_eq!(elapsed, Duration::from_millis(check_ms));
        }
        assert_eq!(catalog_exit_poll_interval(Duration::from_micros(9500)), Duration::from_micros(500));
        assert_eq!(catalog_exit_poll_interval(Duration::from_millis(50)), Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capability_is_explicit_and_exact() {
        for (name, provider) in [
            ("codex", Provider::Codex),
            ("grok", Provider::Grok),
        ] {
            assert_eq!(parse_catalog_provider(name), Some(provider));
            assert_eq!(provider_wire(provider), name);
            assert!(catalog_source(provider).is_some());
        }
        // Claude Code and Pi catalogs are read through their contract leaves.
        for name in ["", "cloud", "pi", "claude", "claude_code", "cursor", "cursor_agent", "agent", "claude-code", "copilot", "gemini_cli", "fake", "Pi", "CODEX", " pi", "pi ", "pi/model", "codex:model", "claude_code_extra"] {
            assert_eq!(parse_catalog_provider(name), None);
        }
        for provider in [Provider::Claude, Provider::ClaudeCode, Provider::Pi, Provider::Copilot, Provider::GeminiCli, Provider::Fake] {
            assert!(catalog_source(provider).is_none());
        }
    }

    #[test]
    fn codex_visibility_and_validation_are_fail_closed() {
        let bytes = br#"{"models":[{"slug":"gpt-6-luna","display_name":"GPT Luna","visibility":"list"},{"slug":"hidden","display_name":"Hidden","visibility":"hide"}]}"#;
        let rows = parse_codex_catalog(bytes, "codex").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "gpt-6-luna");
        assert_eq!(rows[0].vendor, "openai");
        let duplicate = br#"{"models":[{"slug":"x","display_name":"X","visibility":"hide"},{"slug":"x","display_name":"X","visibility":"list"}]}"#;
        assert!(matches!(parse_codex_catalog(duplicate, "codex"), Err(CatalogError::DuplicateIdentity(_))));
    }

    #[test]
    fn matcher_uses_and_tokens_and_all_catalog_fields() {
        let row = ModelRecord { provider: "codex".into(), vendor: "openai".into(), id: "gpt-6-luna".into(), display_name: "GPT Luna".into(), default: None, aliases: vec![] };
        assert!(model_matches(&row, "LUNA GPT"));
        assert!(!model_matches(&row, "LUNA absent"));
        assert!(model_matches(&row, "  "));
    }

}
