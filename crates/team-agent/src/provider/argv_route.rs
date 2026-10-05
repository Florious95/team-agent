//! Global, opt-in native CLI argv routing. No workspace state or child processes.

use super::{wire, Provider};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const ENV_NAME: &str = "TEAM_AGENT_CLI_ARGV_ROUTING";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_providers")]
    pub providers: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            providers: BTreeMap::new(),
        }
    }
}

// serde's struct decoder rejects duplicate top-level fields. A map needs its
// own visitor to reject duplicate provider keys rather than silently last-win.
fn deserialize_providers<'de, D>(decoder: D) -> Result<BTreeMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ProvidersVisitor;
    impl<'de> Visitor<'de> for ProvidersVisitor {
        type Value = BTreeMap<String, Vec<String>>;
        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("unique canonical provider keys with string arrays")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Vec<String>>()? {
                if result.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate provider key"));
                }
            }
            Ok(result)
        }
    }
    decoder.deserialize_map(ProvidersVisitor)
}

pub fn route_key(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Claude | Provider::ClaudeCode => Some("claude"),
        Provider::Fake => None,
        other => Some(wire::provider_wire(other)),
    }
}

pub fn parse_route_provider(raw: &str) -> Option<Provider> {
    let provider = if raw == "cursor" {
        Provider::CursorAgent
    } else {
        wire::parse_provider(raw)?
    };
    route_key(provider).map(|_| provider)
}

pub fn valid_tokens(tokens: &[String]) -> bool {
    tokens.iter().all(|token| !token.contains('\0'))
}

impl Config {
    pub fn parse(bytes: &[u8], path: &Path) -> Result<Self, RouteError> {
        let config: Self = serde_json::from_slice(bytes).map_err(|_| RouteError::invalid(path))?;
        if config.schema_version != 1
            || config.providers.iter().any(|(key, tokens)| {
                parse_route_provider(key).and_then(route_key) != Some(key.as_str())
                    || !valid_tokens(tokens)
            })
        {
            return Err(RouteError::invalid(path));
        }
        Ok(config)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Override {
    Unset,
    On,
    Off,
    Invalid,
}

impl Override {
    pub fn parse(raw: Option<&OsStr>) -> Self {
        match raw {
            None => Self::Unset,
            Some(raw) => match raw
                .to_str()
                .map(|s| s.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("1" | "true" | "on") => Self::On,
                Some("0" | "false" | "off") => Self::Off,
                _ => Self::Invalid,
            },
        }
    }
    pub fn current() -> Self {
        Self::parse(std::env::var_os(ENV_NAME).as_deref())
    }
    pub fn status(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::On => "on",
            Self::Off => "off",
            Self::Invalid => "invalid",
        }
    }
    pub fn effective(self, persisted: bool) -> bool {
        match self {
            Self::On => true,
            Self::Unset => persisted,
            Self::Off | Self::Invalid => false,
        }
    }
    pub fn source(self, file_present: bool) -> &'static str {
        match self {
            Self::Unset if file_present => "config",
            Self::Unset => "default",
            _ => "env",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteError {
    pub error: String,
    pub reason: &'static str,
    pub action: String,
    pub config_path: Option<PathBuf>,
}

impl RouteError {
    fn new(reason: &'static str, message: &str, path: Option<&Path>) -> Self {
        Self {
            error: message.to_string(), reason, config_path: path.map(Path::to_path_buf),
            action: format!("inspect/fix the argv-routing configuration; to force native argv temporarily set {ENV_NAME}=off"),
        }
    }
    fn invalid(path: &Path) -> Self {
        Self::new(
            "argv_route_config_invalid",
            "invalid argv-routing schema, provider mapping or token",
            Some(path),
        )
    }
    fn io(path: &Path, error: io::Error) -> Self {
        Self::new(
            "argv_route_config_unreadable",
            &format!("argv-routing I/O failed ({:?})", error.kind()),
            Some(path),
        )
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.error)?;
        if let Some(path) = &self.config_path {
            write!(formatter, " (config: {})", path.display())?;
        }
        write!(formatter, "; action: {}", self.action)
    }
}
impl std::error::Error for RouteError {}

pub fn config_path() -> Result<PathBuf, RouteError> {
    fn absolute_home(raw: Option<std::ffi::OsString>) -> Option<PathBuf> {
        raw.map(PathBuf::from)
            .filter(|path| path.is_absolute() && !path.as_os_str().is_empty())
    }
    let home = absolute_home(std::env::var_os("HOME"));
    #[cfg(windows)]
    let home = home.or_else(|| absolute_home(std::env::var_os("USERPROFILE")));
    home.map(|path| path.join(".team-agent").join("argv-routing.json"))
        .ok_or_else(|| {
            RouteError::new(
                "argv_route_home_unavailable",
                "an absolute nonempty host home is required",
                None,
            )
        })
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, RouteError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling symlink is an existing broken configuration, not a
            // missing file that management may silently replace.
            if fs::symlink_metadata(path).is_ok() {
                Err(RouteError::io(path, error))
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(RouteError::io(path, error)),
    }
}

pub fn read_config(path: &Path) -> Result<(Config, bool), RouteError> {
    match read_bytes(path)? {
        Some(bytes) => Config::parse(&bytes, path).map(|config| (config, true)),
        None => Ok((Config::default(), false)),
    }
}

/// Insert literal tokens after the executable, preserving the entire old tail.
pub fn insert(config: &Config, provider: Provider, argv: &mut Vec<String>) {
    if let Some(tokens) = route_key(provider).and_then(|key| config.providers.get(key)) {
        if !argv.is_empty() && !tokens.is_empty() {
            argv.splice(1..1, tokens.iter().cloned());
        }
    }
}

/// One read per actual native launch. OFF never reads or writes configuration.
pub fn apply(provider: Provider, argv: &mut Vec<String>) -> Result<(), RouteError> {
    let override_state = Override::current();
    if route_key(provider).is_none() || matches!(override_state, Override::Off | Override::Invalid)
    {
        return Ok(());
    }
    let path = match config_path() {
        Ok(path) => path,
        Err(_) if override_state == Override::Unset => return Ok(()),
        Err(error) => return Err(error),
    };
    let bytes = match read_bytes(&path) {
        Ok(bytes) => bytes,
        Err(_) if override_state == Override::Unset => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(bytes) = bytes else {
        return Ok(());
    };
    if override_state == Override::Unset {
        // Tolerant OFF detection precedes strict mapping validation. Never
        // make a disabled or unrecognizable file block historical launches.
        let enabled = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("enabled").and_then(serde_json::Value::as_bool));
        if enabled != Some(true) {
            return Ok(());
        }
    }
    insert(&Config::parse(&bytes, &path)?, provider, argv);
    Ok(())
}

pub enum Mutation {
    Enable(bool),
    Set(Provider, Vec<String>),
    Add(Provider, Vec<String>),
    Clear(Provider),
}

struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = crate::platform::file_lock::unlock(&self.0);
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn acquire_lock(path: &Path) -> Result<Lock, RouteError> {
    let parent = path.parent().ok_or_else(|| RouteError::invalid(path))?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && parent.is_dir() => {}
        Err(error) => return Err(RouteError::io(path, error)),
    }
    let file = private_options()
        .create(true)
        .truncate(false)
        .open(parent.join("argv-routing.lock"))
        .map_err(|error| RouteError::io(path, error))?;
    let started = Instant::now();
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => return Ok(Lock(file)),
            Ok(false) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Ok(false) => {
                return Err(RouteError::new(
                    "argv_route_lock_timeout",
                    "argv-routing write lock timed out",
                    Some(path),
                ))
            }
            Err(error) => return Err(RouteError::io(path, error)),
        }
    }
}

fn write_config(path: &Path, config: &Config) -> Result<(), RouteError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let serial = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_file_name(format!(
        ".argv-routing-{}-{nanos}-{serial}.tmp",
        std::process::id()
    ));
    // Only remove a temp path after successfully creating it ourselves.
    let mut file = private_options()
        .create_new(true)
        .open(&temp)
        .map_err(|error| RouteError::io(path, error))?;
    let result = (|| {
        serde_json::to_writer_pretty(&mut file, config).map_err(|_| RouteError::invalid(path))?;
        file.write_all(b"\n")
            .and_then(|_| file.flush())
            .and_then(|_| file.sync_all())
            .map_err(|error| RouteError::io(path, error))?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| RouteError::io(path, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn mutate(path: &Path, mutation: Mutation) -> Result<(Config, bool), RouteError> {
    match &mutation {
        Mutation::Set(provider, tokens) | Mutation::Add(provider, tokens) => {
            if route_key(*provider).is_none() || tokens.is_empty() || !valid_tokens(tokens) {
                return Err(RouteError::invalid(path));
            }
        }
        Mutation::Clear(provider) if route_key(*provider).is_none() => {
            return Err(RouteError::invalid(path))
        }
        _ => {}
    }
    // Strict preflight prevents creating lock/dir apparatus for a bad file.
    let (config, present) = read_config(path)?;
    if !present && matches!(mutation, Mutation::Clear(_)) {
        return Ok((config, false));
    }
    let _lock = acquire_lock(path)?;
    let (mut config, present) = read_config(path)?;
    match mutation {
        Mutation::Enable(enabled) => config.enabled = enabled,
        Mutation::Set(provider, tokens) => {
            if let Some(key) = route_key(provider) {
                config.providers.insert(key.to_string(), tokens);
            }
        }
        Mutation::Add(provider, tokens) => {
            if let Some(key) = route_key(provider) {
                config
                    .providers
                    .entry(key.to_string())
                    .or_default()
                    .extend(tokens);
            }
        }
        Mutation::Clear(provider) => {
            if let Some(key) = route_key(provider) {
                if config.providers.remove(key).is_none() {
                    return Ok((config, present));
                }
            }
        }
    }
    write_config(path, &config)?;
    Ok((config, true))
}
