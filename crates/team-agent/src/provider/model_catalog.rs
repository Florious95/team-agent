//! Provider model discovery, bounded native catalog queries, and shared fuzzy search.
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::model::enums::Provider;

const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CATALOG_BYTES: u64 = 1024 * 1024;
const CLAUDE_REQUEST_ID: &str = "team-agent-model-catalog";
static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
            "cursor_agent" => "agent",
            "codex" => "codex",
            "claude" | "claude_code" => "claude",
            "grok" => "grok",
            _ => "provider CLI",
        };
        match self {
            Self::UnsupportedProvider => "choose a supported provider: pi, cursor_agent, codex, claude, claude_code, or grok".into(),
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
        "cursor_agent" => "Cursor",
        "codex" => "Codex",
        "claude" | "claude_code" => "Claude",
        "grok" => "Grok",
        _ => provider,
    }
}

/// Only the documented catalog names are accepted; launch aliases stay unchanged.
pub fn parse_catalog_provider(name: &str) -> Option<Provider> {
    match name {
        "pi" => Some(Provider::Pi),
        "cursor_agent" => Some(Provider::CursorAgent),
        "codex" => Some(Provider::Codex),
        "claude" => Some(Provider::Claude),
        "claude_code" => Some(Provider::ClaudeCode),
        "grok" => Some(Provider::Grok),
        _ => None,
    }
}

fn provider_wire_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Pi => "pi",
        Provider::CursorAgent => "cursor_agent",
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini_cli",
        Provider::Grok => "grok",
        Provider::Fake => "fake",
    }
}

#[derive(Clone, Copy)]
enum CatalogSource {
    Pi,
    CursorAgent,
    Codex,
    Grok,
    Claude,
}

// Exhaustive by design: a new Provider variant must make an explicit catalog decision.
fn catalog_source(provider: Provider) -> Option<CatalogSource> {
    match provider {
        Provider::Pi => Some(CatalogSource::Pi),
        Provider::CursorAgent => Some(CatalogSource::CursorAgent),
        Provider::Codex => Some(CatalogSource::Codex),
        Provider::Claude | Provider::ClaudeCode => Some(CatalogSource::Claude),
        Provider::Grok => Some(CatalogSource::Grok),
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => None,
    }
}

pub fn discover_model_catalog(provider_name: &str) -> Result<Vec<ModelRecord>, CatalogError> {
    let provider = parse_catalog_provider(provider_name).ok_or(CatalogError::UnsupportedProvider)?;
    let source = catalog_source(provider).ok_or(CatalogError::UnsupportedProvider)?;
    let requested_provider = provider_wire_name(provider);
    let records = match source {
        CatalogSource::Pi => {
            let bytes = run_native("pi", &["--list-models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_pi_catalog(&bytes)?
        }
        CatalogSource::CursorAgent => {
            let bytes = run_native("agent", &["--list-models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_cursor_catalog(&bytes)?
        }
        CatalogSource::Codex => {
            let bytes = run_native("codex", &["debug", "models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_codex_catalog(&bytes, requested_provider)?
        }
        CatalogSource::Grok => {
            let bytes = run_native("grok", &["models"], None, None, &[], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
            parse_grok_catalog(&bytes)?
        }
        CatalogSource::Claude => discover_claude_catalog(requested_provider)?,
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

pub(crate) fn parse_cursor_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, CatalogError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidUtf8)?;
    let lines = text.lines().collect::<Vec<_>>();
    let tip_line = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .filter(|index| {
            lines[*index]
                .strip_prefix("Tip: use --model ")
                .is_some_and(|tip| !tip.is_empty())
        });
    let mut records: Vec<ModelRecord> = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if Some(index) == tip_line {
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.trim_end_matches(':').eq_ignore_ascii_case("available models") { continue; }
        let Some((id, display)) = line.split_once(" - ") else { return Err(CatalogError::Malformed); };
        let id = id.trim();
        if id.is_empty() || id.chars().any(char::is_whitespace) || display.trim().is_empty() { return Err(CatalogError::Malformed); }
        if records.iter().any(|record| record.id == id) { return Err(CatalogError::DuplicateIdentity(id.into())); }
        let display = display.trim();
        let default = display.ends_with("(default)");
        let display_name = display.strip_suffix("(default)").map_or(display, str::trim).to_string();
        let vendor = id.split_once('-').map_or("cursor", |(vendor, _)| vendor).to_string();
        records.push(ModelRecord { provider: "cursor_agent".into(), vendor, id: id.into(), display_name, default: Some(default), aliases: Vec::new() });
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

fn discover_claude_catalog(provider: &str) -> Result<Vec<ModelRecord>, CatalogError> {
    let cwd = OwnedTempDir::create()?;
    let args = [
        "--print", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
        "--no-session-persistence", "--strict-mcp-config", "--mcp-config", r#"{"mcpServers":{}}"#,
        "--setting-sources", "", "--settings", r#"{"disableAllHooks":true}"#, "--tools", "", "--no-chrome",
    ];
    let request = serde_json::json!({"type":"control_request","request_id":CLAUDE_REQUEST_ID,"request":{"subtype":"initialize","hooks":{},"agents":{},"skills":[]}});
    let mut input = serde_json::to_vec(&request).map_err(|_| CatalogError::InvalidProtocol)?;
    input.push(b'\n');
    let bytes = run_native("claude", &args, Some(&input), Some(&cwd.0), &["CLAUDECODE"], CATALOG_TIMEOUT, MAX_CATALOG_BYTES, &mut NoopObserver)?;
    parse_claude_catalog(&bytes, provider)
}

fn parse_claude_catalog(bytes: &[u8], provider: &str) -> Result<Vec<ModelRecord>, CatalogError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CatalogError::InvalidUtf8)?;
    let mut response = None;
    for line in text.lines() {
        if line.is_empty() { return Err(CatalogError::InvalidProtocol); }
        let frame: serde_json::Value = serde_json::from_str(line).map_err(|_| CatalogError::InvalidProtocol)?;
        let frame_type = frame
            .get("type")
            .and_then(serde_json::Value::as_str)
            .filter(|frame_type| !frame_type.is_empty())
            .ok_or(CatalogError::InvalidProtocol)?;
        if frame_type != "control_response" {
            continue;
        }
        if response.is_some() {
            return Err(CatalogError::InvalidProtocol);
        }
        let result = frame.get("response").ok_or(CatalogError::UnsupportedSchema)?;
        let outer_request_id = frame.get("request_id").and_then(serde_json::Value::as_str);
        let inner_request_id = result.get("request_id").and_then(serde_json::Value::as_str);
        if (outer_request_id != Some(CLAUDE_REQUEST_ID) && inner_request_id != Some(CLAUDE_REQUEST_ID))
            || frame.get("request_id").is_some_and(|id| id.as_str() != Some(CLAUDE_REQUEST_ID))
            || result.get("request_id").is_some_and(|id| id.as_str() != Some(CLAUDE_REQUEST_ID))
        {
            return Err(CatalogError::InvalidProtocol);
        }
        if result.get("subtype").and_then(serde_json::Value::as_str) != Some("success") { return Err(CatalogError::InvalidProtocol); }
        let rows = result.get("response").and_then(|v| v.get("models")).and_then(serde_json::Value::as_array).ok_or(CatalogError::UnsupportedSchema)?;
        response = Some(rows.clone());
    }
    let rows = response.ok_or(CatalogError::InvalidProtocol)?;
    if rows.is_empty() { return Err(CatalogError::Empty); }
    let mut source_values = HashSet::new();
    let mut by_id = HashMap::<String, usize>::new();
    let mut records: Vec<ModelRecord> = Vec::new();
    let mut group_defaults = Vec::<bool>::new();
    let mut group_has_non_default_display = Vec::<bool>::new();
    let mut any_default = false;
    for row in rows {
        let row = row.as_object().ok_or(CatalogError::Malformed)?;
        let value = required_string(row.get("value"))?;
        let resolved = required_string(row.get("resolvedModel")).map_err(|_| CatalogError::UnsupportedSchema)?;
        let display = required_string(row.get("displayName")).map_err(|_| CatalogError::UnsupportedSchema)?;
        validate_identity(value)?;
        validate_identity(resolved)?;
        validate_display(display)?;
        if !source_values.insert(value.to_string()) { return Err(CatalogError::DuplicateIdentity(value.into())); }
        let is_default = value == "default";
        any_default |= is_default;
        if let Some(index) = by_id.get(resolved).copied() {
            let record = &mut records[index];
            record.aliases.push(value.into());
            if !is_default && !group_has_non_default_display[index] {
                record.display_name = display.into();
                group_has_non_default_display[index] = true;
            }
            group_defaults[index] |= is_default;
        } else {
            by_id.insert(resolved.into(), records.len());
            records.push(ModelRecord { provider: provider.into(), vendor: "anthropic".into(), id: resolved.into(), display_name: display.into(), default: None, aliases: vec![value.into()] });
            group_defaults.push(is_default);
            group_has_non_default_display.push(!is_default);
        }
    }
    for (record, is_default) in records.iter_mut().zip(group_defaults) {
        record.default = any_default.then_some(is_default);
    }
    Ok(records)
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
    let mut too_large_at = None;
    let status = loop {
        match reader_rx.try_recv() {
            Ok(Ok(bytes)) => {
                if bytes.len() as u64 > max_bytes {
                    too_large_at.get_or_insert_with(Instant::now);
                } else {
                    output = Some(bytes);
                }
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
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
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

fn terminate(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    unsafe {
        // The process group was created for this one native catalog invocation.
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn finish_stdout_after_exit(
    stdout: std::process::ChildStdout,
    status: ExitStatus,
    timeout: Duration,
    max_bytes: u64,
    observer: &mut dyn CatalogObserver,
) -> Result<Vec<u8>, CatalogError> {
    if !status.success() { observer.exited(&status); return Err(CatalogError::CommandFailed); }
    observer.exited(&status);
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().name("model-catalog-reader".into()).spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes);
        let _ = tx.send(result.map(|_| bytes));
    }).map_err(|_| CatalogError::OutputReadFailed)?;
    let result = match rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) if bytes.len() as u64 <= max_bytes => Ok(bytes),
        Ok(Ok(_)) => Err(CatalogError::OutputTooLarge),
        Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(CatalogError::OutputReadFailed),
        Err(mpsc::RecvTimeoutError::Timeout) => { observer.output_timeout(); Err(CatalogError::Timeout) }
    };
    if !matches!(&result, Err(CatalogError::Timeout)) { let _ = reader.join(); }
    result
}

struct OwnedTempDir(PathBuf);
impl OwnedTempDir {
    fn create() -> Result<Self, CatalogError> {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir();
        for _ in 0..8 {
            let id = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("team-agent-claude-models-{}-{id}", std::process::id()));
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(CatalogError::TemporaryDirectoryUnavailable),
            }
        }
        Err(CatalogError::TemporaryDirectoryUnavailable)
    }
}
impl Drop for OwnedTempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capability_is_explicit_and_exact() {
        assert_eq!(parse_catalog_provider("claude_code"), Some(Provider::ClaudeCode));
        assert_eq!(parse_catalog_provider("cloud"), None);
        assert!(catalog_source(Provider::Codex).is_some());
        assert!(catalog_source(Provider::Fake).is_none());
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
    fn claude_selectors_group_by_exact_resolved_id() {
        let response = serde_json::json!({"type":"control_response","request_id":CLAUDE_REQUEST_ID,"response":{"subtype":"success","response":{"models":[
            {"value":"default","resolvedModel":"claude-opus-5-5[1m]","displayName":"Default"},
            {"value":"opus[1m]","resolvedModel":"claude-opus-5-5[1m]","displayName":"Opus 1M"},
            {"value":"opus-alt","resolvedModel":"claude-opus-5-5[1m]","displayName":"Alternate display"},
            {"value":"sonnet","resolvedModel":"claude-sonnet-5","displayName":"Sonnet"}
        ]}}});
        let bytes = format!("{}\n", response).into_bytes();
        let rows = parse_claude_catalog(&bytes, "claude_code").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].provider, "claude_code");
        assert_eq!(rows[0].id, "claude-opus-5-5[1m]");
        assert_eq!(rows[0].display_name, "Opus 1M");
        assert_eq!(rows[0].aliases, vec!["default", "opus[1m]", "opus-alt"]);
        assert_eq!(rows[0].default, Some(true));
        assert_eq!(rows[1].default, Some(false));
        assert!(model_matches(&rows[0], "OPUS 1M"));
    }

    #[test]
    fn claude_ignores_other_stream_frames_and_matches_nested_request_id() {
        let models = serde_json::json!([{"value":"default","resolvedModel":"claude-sonnet-5","displayName":"Sonnet"}]);
        let frames = [
            serde_json::json!({"type":"system","subtype":"hook_started"}),
            serde_json::json!({"type":"system","subtype":"hook_response"}),
            serde_json::json!({"type":"stream_event","event":{"type":"message_start"}}),
            serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":CLAUDE_REQUEST_ID,"response":{"models":models}}}),
        ];
        let bytes = frames.iter().map(serde_json::Value::to_string).collect::<Vec<_>>().join("\n");
        let rows = parse_claude_catalog(bytes.as_bytes(), "claude_code").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "claude-sonnet-5");

        let conflicting = serde_json::json!({"type":"control_response","request_id":CLAUDE_REQUEST_ID,"response":{"subtype":"success","request_id":"wrong-id","response":{"models":[{"value":"default","resolvedModel":"claude-sonnet-5","displayName":"Sonnet"}]}}});
        assert!(matches!(parse_claude_catalog(format!("{}\n", conflicting).as_bytes(), "claude_code"), Err(CatalogError::InvalidProtocol)));
    }

    #[test]
    fn claude_missing_canonical_id_and_duplicate_selectors_fail_closed() {
        let missing = serde_json::json!({"type":"control_response","request_id":CLAUDE_REQUEST_ID,"response":{"subtype":"success","response":{"models":[{"value":"default","displayName":"Default"}]}}});
        assert!(matches!(parse_claude_catalog(format!("{}\n", missing).as_bytes(), "claude"), Err(CatalogError::UnsupportedSchema)));
        let duplicate = serde_json::json!({"type":"control_response","request_id":CLAUDE_REQUEST_ID,"response":{"subtype":"success","response":{"models":[
            {"value":"opus","resolvedModel":"claude-opus","displayName":"Opus"},
            {"value":"opus","resolvedModel":"claude-opus[1m]","displayName":"Opus 1M"}
        ]}}});
        assert!(matches!(parse_claude_catalog(format!("{}\n", duplicate).as_bytes(), "claude"), Err(CatalogError::DuplicateIdentity(_))));
    }

    #[test]
    fn matcher_uses_and_tokens_and_all_catalog_fields() {
        let row = ModelRecord { provider: "codex".into(), vendor: "openai".into(), id: "gpt-6-luna".into(), display_name: "GPT Luna".into(), default: None, aliases: vec![] };
        assert!(model_matches(&row, "LUNA GPT"));
        assert!(!model_matches(&row, "LUNA absent"));
        assert!(model_matches(&row, "  "));
    }

    #[test]
    fn frozen_cursor_catalog_skips_only_its_native_trailing_tip() {
        let frozen = include_bytes!("testdata/cursor-list-models-native.stdout");
        let records = parse_cursor_catalog(frozen).unwrap();
        let low = records.iter().find(|row| row.id == "gpt-5.6-luna-low").unwrap();
        assert_eq!(low.provider.as_str(), "cursor_agent");
        assert_eq!(low.vendor.as_str(), "gpt");
        assert_eq!(low.display_name, "GPT-5.6 Luna 1M Low (current)");
        assert_eq!(low.default, Some(false));
        assert!(low.aliases.is_empty());
        let xhigh = records.iter().find(|row| row.id == "gpt-5.6-luna-xhigh").unwrap();
        assert_eq!(xhigh.id.as_str(), "gpt-5.6-luna-xhigh");
        assert_eq!(xhigh.display_name, "GPT-5.6 Luna 1M Extra High");
        let auto = records.iter().find(|row| row.id == "auto").unwrap();
        assert_eq!(auto.default, Some(true));

        let text = std::str::from_utf8(frozen).unwrap();
        let without_tip = text.split_once("\nTip: use --model ").unwrap().0;
        assert_eq!(parse_cursor_catalog(without_tip.as_bytes()).unwrap(), parse_cursor_catalog(frozen).unwrap());
    }

    #[test]
    fn cursor_tip_compatibility_keeps_fail_closed_row_validation() {
        let duplicate = b"Available models\na - A\na - A\n";
        assert!(matches!(parse_cursor_catalog(duplicate), Err(CatalogError::DuplicateIdentity(_))));
        assert!(matches!(parse_cursor_catalog(b"Available models\n"), Err(CatalogError::Empty)));
        assert!(matches!(parse_cursor_catalog(&[b'A', 0xff]), Err(CatalogError::InvalidUtf8)));
        assert!(matches!(parse_cursor_catalog(b"Available models\na - A\nnot a model row\n"), Err(CatalogError::Malformed)));
        assert!(matches!(parse_cursor_catalog(b"Available models\na - A\nTip: use --model a to switch.\nunexpected tail\n"), Err(CatalogError::Malformed)));
    }
}
