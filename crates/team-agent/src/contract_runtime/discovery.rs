//! Bounded native discovery. Nothing here creates a runtime or claims T2/T3.
use std::path::{Path, PathBuf};

use team_agent_contract::contract::{hooks::*, plan::CatalogObservation, types::*};
use team_agent_contract::host::{command::{PublicCommand, RealCommandRunner}, process::resolve_cwd};
use team_agent_contract::kiro::native as kiro;
use team_agent_contract::providers::claude::{catalog as claude_catalog, launcher as claude};
use team_agent_contract::providers::codex::{catalog as codex_catalog, launcher as codex};
use team_agent_contract::providers::cursor::native as cursor;
use team_agent_contract::providers::grok_build::native as grok;
use team_agent_contract::providers::pi::{catalog as pi_catalog, launcher as pi};
use thiserror::Error;

use super::registry::{self, NativeProvider, NativeRuntime};

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("required executable is unavailable: {0}")]
    Executable(&'static str),
    #[error("native discovery failed: {0}")]
    Host(#[from] team_agent_contract::host::HostError),
    #[error("native engine probe failed: {0}")]
    Probe(#[from] kiro::EngineProbeError),
    #[error("native Cursor launcher probe failed: {0}")]
    CursorProbe(#[from] cursor::ProbeError),
    #[error("native Grok launcher probe failed: {0}")]
    GrokProbe(#[from] grok::ProbeError),
    #[error("native Codex launcher probe failed: {0}")]
    CodexProbe(#[from] codex_catalog::ProbeError),
    #[error("native Pi launcher probe failed: {0}")]
    PiProbe(#[from] pi_catalog::ProbeError),
    #[error("native Claude launcher probe failed: {0}")]
    ClaudeProbe(#[from] claude_catalog::ProbeError),
    #[error("native discovery contract: {0:?}")]
    Contract(ContractError),
    #[error("native model discovery: {0:?}")]
    Catalog(ReadFailure),
    #[error("native discovery I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Discovery {
    /// The provider's public command exactly as the user's shell resolves it.
    pub command: PublicCommand,
    pub engine: PathBuf,
    pub native: NativeIdentity,
    pub catalog: CatalogObservation,
    pub runtime: Option<NativeRuntime>,
}

/// Canonical tool path for framework infrastructure such as tmux. Providers
/// never use this: they launch their public command through [`public_command`].
pub fn executable(name: &'static str) -> Result<PathBuf, DiscoveryError> {
    let path = std::env::var_os("PATH").ok_or(DiscoveryError::Executable(name))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        use std::os::unix::fs::PermissionsExt;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok(candidate.canonicalize()?);
        }
    }
    Err(DiscoveryError::Executable(name))
}

/// The first `name` on this process's PATH, kept as found so user wrappers and
/// vendor symlinks stay in the route. No install directory is ever guessed.
pub fn public_command(name: &'static str) -> Result<PublicCommand, DiscoveryError> {
    let path = std::env::var_os("PATH").ok_or(DiscoveryError::Executable(name))?;
    PublicCommand::resolve(name, &path).map_err(|_| DiscoveryError::Executable(name))
}

pub fn discover(provider: &ProviderId, workspace: &Path) -> Result<Discovery, DiscoveryError> {
    let selected = registry::provider(provider.as_str()).map_err(DiscoveryError::Contract)?;
    let descriptor = selected.descriptor();
    let bounds = ReadBounds {
        deadline: std::time::Duration::from_secs(30),
        max_output_bytes: 1024 * 1024,
    };
    // Every metadata probe runs the PATH-selected entry, wrappers included.
    let command = public_command(descriptor.identity.binary)?;
    let native = match selected {
        NativeProvider::Kiro => kiro::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
        NativeProvider::Cursor => cursor::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
        NativeProvider::GrokBuild => grok::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
        NativeProvider::Codex => codex_catalog::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
        NativeProvider::Pi => pi_catalog::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
        NativeProvider::Claude => claude_catalog::probe_launcher(&command, &mut RealCommandRunner::default(), bounds)?,
    };
    let request = CatalogRequest {
        provider: ProviderId::new(descriptor.identity.id).map_err(DiscoveryError::Contract)?,
        native: native.clone(),
        executable: command.entry.clone(),
        cwd: resolve_cwd(workspace)?,
        source: descriptor.model.catalog.require("native catalog source").map_err(DiscoveryError::Contract)?.clone(),
        bounds,
    };
    // H1 does not use these pointers or execute an MCP server. The launcher
    // fingerprint authorizes only metadata reads, never worker launch.
    let adapter = selected.adapter(
        std::env::current_exe()?.canonicalize()?,
        Vec::new(),
        Default::default(),
    ).map_err(DiscoveryError::Contract)?;
    let hooks = adapter.hooks();
    let catalog_hook = hooks.catalog.require("H1 catalog").map_err(DiscoveryError::Contract)?;
    let catalog = match selected {
        NativeProvider::Kiro => {
            let mut host = kiro::CatalogReader::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
        NativeProvider::Cursor => {
            let mut host = cursor::NativeCatalog::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
        NativeProvider::GrokBuild => {
            let mut host = grok::NativeCatalog::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
        NativeProvider::Codex => {
            let mut host = codex_catalog::NativeCatalog::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
        NativeProvider::Pi => {
            let mut host = pi_catalog::NativeCatalog::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
        NativeProvider::Claude => {
            let mut host = claude_catalog::NativeCatalog::new(request.clone(), RealCommandRunner::default())
                .map_err(DiscoveryError::Contract)?;
            catalog_hook.discover(&request, &mut host)
        }
    }.map_err(DiscoveryError::Catalog)?;
    let result = Discovery { engine: command.entry.clone(), command, native, catalog, runtime: None };
    validate_catalog(provider, &result).map_err(DiscoveryError::Contract)?;
    Ok(result)
}

/// Runtime discovery is separate from public catalog metadata. Each provider
/// bridges its catalog to the native image its public command route execs into.
pub(super) fn discover_runtime(provider: &ProviderId, workspace: &Path) -> Result<Discovery, DiscoveryError> {
    let mut discovery = discover(provider, workspace)?;
    let selected = registry::provider(provider.as_str()).map_err(DiscoveryError::Contract)?;
    let bounds = ReadBounds { deadline: std::time::Duration::from_secs(30), max_output_bytes: 1024 * 1024 };
    let home = || std::env::var_os("HOME").map(PathBuf::from)
        .ok_or(DiscoveryError::Executable("HOME for native subscription"));
    let runtime = match selected {
        NativeProvider::Kiro => NativeRuntime::Kiro(
            kiro::probe_runtime(&discovery.command, &mut RealCommandRunner::default(), bounds)?),
        NativeProvider::Cursor => NativeRuntime::Cursor(
            cursor::probe_runtime(&discovery.command, &home()?, &mut RealCommandRunner::default(), bounds)?),
        NativeProvider::GrokBuild => NativeRuntime::GrokBuild(
            grok::probe_runtime(&discovery.command, &home()?, &mut RealCommandRunner::default(), bounds)?),
        NativeProvider::Codex => NativeRuntime::Codex(
            codex::probe_runtime(&discovery.command, &home()?, &mut RealCommandRunner::default(), bounds)?),
        NativeProvider::Pi => NativeRuntime::Pi(
            pi::probe_runtime(&discovery.command, &home()?, &mut RealCommandRunner::default(), bounds)?),
        NativeProvider::Claude => NativeRuntime::Claude(
            claude::probe_runtime(&discovery.command, &home()?, &mut RealCommandRunner::default(), bounds)?),
    };
    discovery.catalog = runtime.bind_catalog(&discovery.catalog).map_err(DiscoveryError::Contract)?;
    discovery.engine = runtime.engine().clone();
    discovery.native = runtime.native().clone();
    discovery.runtime = Some(runtime);
    Ok(discovery)
}

fn validate_catalog(provider: &ProviderId, discovery: &Discovery) -> Result<(), ContractError> {
    let descriptor = registry::provider(provider.as_str())?.descriptor();
    if discovery.catalog.provider.as_str() != descriptor.identity.id
        || discovery.catalog.native != discovery.native
        || discovery.catalog.schema != descriptor.model.catalog.require("native catalog source")?.schema
    {
        return Err(ContractError::Mismatch("native catalog provider/identity/schema"));
    }
    Ok(())
}

pub fn models(
    args: &crate::cli::ModelsArgs,
) -> Result<crate::cli::CmdResult, crate::cli::CliError> {
    let result = std::env::current_dir()
        .map_err(DiscoveryError::Io)
        .and_then(|cwd| {
            let provider = ProviderId::new(&args.provider).map_err(DiscoveryError::Contract)?;
            discover(&provider, &cwd)
        });
    render_models(args, result)
}

fn render_models(
    args: &crate::cli::ModelsArgs,
    result: Result<Discovery, DiscoveryError>,
) -> Result<crate::cli::CmdResult, crate::cli::CliError> {
    use crate::cli::{CmdOutput, CmdResult, ExitCode};
    use serde_json::json;
    let result = result.and_then(|discovery| {
        let provider = ProviderId::new(&args.provider).map_err(DiscoveryError::Contract)?;
        validate_catalog(&provider, &discovery).map_err(DiscoveryError::Contract)?;
        Ok(discovery)
    });
    let (value, human, exit) = match result {
        Ok(discovery) => {
            let query = args.search.as_deref().map(str::to_lowercase);
            let rows: Vec<_> = discovery.catalog.models.iter()
                .filter(|model| query.as_ref().is_none_or(|query| model.id.to_lowercase().contains(query)))
                .map(|model| {
                    let efforts = match &model.efforts {
                        Support::Supported(values) => json!({"state":"supported","values":values.iter().map(|value| value.as_str()).collect::<Vec<_>>()}),
                        Support::Unsupported(reason) => json!({"state":"unsupported","reason":reason.code}),
                        Support::Unverified(reason) => json!({"state":"unverified","reason":reason.code}),
                    };
                    json!({"provider":args.provider,"model_id":model.id,"role_model":model.id,"aliases":[],"efforts":efforts})
                }).collect();
            let value = json!({"schema_version":"models.v1","ok":true,"provider":args.provider,
                "models":rows,"auth":"unknown","auth_basis":"native_catalog_visibility_is_not_authorization",
                "source":{"schema":discovery.catalog.schema,"executable":discovery.engine,
                    "version":discovery.native.version,"executable_sha256":team_agent_contract::host::digest_hex(discovery.native.executable_sha256),
                    "identity_scope":"catalog_only"},
                "current_role_model":null,"search":args.search});
            let mut lines = vec![format!("models.v1 | {} native model IDs:", args.provider)];
            lines.extend(
                rows.iter()
                    .filter_map(|row| row["model_id"].as_str())
                    .map(str::to_owned),
            );
            lines.push("Catalog visibility does not prove subscription authorization, runtime model selection, or effort support.".into());
            (value, lines.join("\n"), ExitCode::Ok)
        }
        Err(error) => {
            let message = error.to_string();
            let action = "Check the selected native executable and existing subscription; no model fallback or login retry was performed.";
            (
                json!({"schema_version":"models.v1","ok":false,"provider":args.provider,"models":[],"auth":"not_ready","error":message,"action":action}),
                format!("models.v1 | {message}\n{action}"),
                ExitCode::Error,
            )
        }
    };
    Ok(CmdResult {
        output: CmdOutput::Human(if args.json {
            serde_json::to_string_pretty(&value)?
        } else {
            human
        }),
        exit,
        as_json: false,
        preserve_json_order: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CmdOutput, ExitCode, ModelsArgs};
    use team_agent_contract::contract::plan::ModelRecord;

    fn controlled_command(name: &str) -> PublicCommand {
        PublicCommand { name: name.into(), search_path: "/controlled".into(), entry: format!("/controlled/{name}").into() }
    }

    #[test]
    fn native_catalog_projection_preserves_ids_without_claiming_auth_or_effort() {
        let native = NativeIdentity {
            version: "controlled".into(),
            harness: "v3".into(),
            ui: "tui".into(),
            platform: Platform::Linux,
            executable_sha256: Digest([1; 32]),
        };
        let discovery = Discovery {
            command: controlled_command("kiro-cli"),
            runtime: None,
            engine: "/controlled/kiro-cli".into(),
            native: native.clone(),
            catalog: CatalogObservation {
                provider: ProviderId::new("kiro").unwrap(),
                native,
                schema: kiro::CHAT_CATALOG_SOURCE.schema.into(),
                models: vec![ModelRecord {
                    id: "Exact-Case-ID".into(),
                    efforts: Support::Unverified(Reason {
                        code: "not-observed",
                        message: "No effort evidence",
                    }),
                }],
            },
        };
        let args = ModelsArgs {
            provider: "kiro".into(),
            search: Some("exact".into()),
            json: true,
        };
        let result = render_models(&args, Ok(discovery)).unwrap();
        assert_eq!(result.exit, ExitCode::Ok);
        let CmdOutput::Human(text) = result.output else {
            panic!("JSON text projection");
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["auth"], "unknown");
        assert_eq!(json["models"][0]["model_id"], "Exact-Case-ID");
        assert_eq!(json["models"][0]["efforts"]["state"], "unverified");
        assert_eq!(json["source"]["schema"], kiro::CHAT_CATALOG_SOURCE.schema);
        assert!(json["current_role_model"].is_null());
    }

    #[test]
    fn version_failure_projection_preserves_the_received_version() {
        let args = ModelsArgs {
            provider: "kiro".into(),
            search: None,
            json: true,
        };
        let error = kiro::parse_version(b"kiro-cli-chat 2.29.invalid\n").unwrap_err();
        let result = render_models(&args, Err(DiscoveryError::Probe(error))).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Human(text) = result.output else {
            panic!("JSON text projection");
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("kiro-cli-chat 2.29.invalid"));
        assert_eq!(json["models"], serde_json::json!([]));
    }

    fn cursor_catalog() -> Discovery {
        let native = NativeIdentity {
            version: "2026.09.29-a1b2c3".into(),
            harness: cursor::METADATA_HARNESS.into(),
            ui: cursor::METADATA_UI.into(),
            platform: Platform::MacOs,
            executable_sha256: Digest([2; 32]),
        };
        Discovery {
            command: controlled_command("cursor-agent"),
            runtime: None,
            engine: "/controlled/cursor-agent".into(),
            native: native.clone(),
            catalog: CatalogObservation {
                provider: ProviderId::new("cursor").unwrap(),
                native,
                schema: cursor::CATALOG_SOURCE.schema.into(),
                models: ["gpt-5.6-luna-xhigh", "gpt-5.6-luna-xhigh-fast", "Other-Exact-ID"]
                    .into_iter().map(|id| ModelRecord {
                        id: id.into(),
                        efforts: Support::Unverified(Reason {
                            code: "not-observed", message: "Native IDs are not separate effort flags",
                        }),
                    }).collect(),
            },
        }
    }

    #[test]
    fn cursor_catalog_filters_exact_native_family_without_runtime_claims() {
        let args = ModelsArgs {
            provider: "cursor".into(), search: Some("GPT-5.6-LUNA-XHIGH".into()), json: true,
        };
        let result = render_models(&args, Ok(cursor_catalog())).unwrap();
        assert_eq!(result.exit, ExitCode::Ok);
        let CmdOutput::Human(text) = result.output else { panic!("JSON text projection"); };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let ids: Vec<_> = json["models"].as_array().unwrap().iter()
            .map(|row| row["model_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["gpt-5.6-luna-xhigh", "gpt-5.6-luna-xhigh-fast"]);
        assert_eq!(json["provider"], "cursor");
        assert_eq!(json["auth"], "unknown");
        assert_eq!(json["source"]["identity_scope"], "catalog_only");
        assert_eq!(json["source"]["version"], "2026.09.29-a1b2c3");
        assert!(json["models"].as_array().unwrap().iter().all(|row|
            row["efforts"]["state"] == "unverified" && row["aliases"] == serde_json::json!([])));
        assert!(json["current_role_model"].is_null());
    }

    #[test]
    fn grok_catalog_alias_uses_the_same_canonical_identity() {
        for provider in ["grok", "grok_build"] {
            let mut discovery = cursor_catalog();
            discovery.catalog.provider = ProviderId::new("grok_build").unwrap();
            discovery.catalog.schema = grok::CATALOG_SOURCE.schema.into();
            let args = ModelsArgs { provider: provider.into(), search: None, json: true };
            assert_eq!(render_models(&args, Ok(discovery)).unwrap().exit, ExitCode::Ok);
        }
    }

    #[test]
    fn mismatched_catalog_provider_native_or_schema_is_not_projected() {
        let args = ModelsArgs { provider: "cursor".into(), search: None, json: true };
        for field in ["provider", "native", "schema"] {
            let mut discovery = cursor_catalog();
            match field {
                "provider" => discovery.catalog.provider = ProviderId::new("kiro").unwrap(),
                "native" => discovery.catalog.native.executable_sha256 = Digest([9; 32]),
                "schema" => discovery.catalog.schema = kiro::CHAT_CATALOG_SOURCE.schema.into(),
                _ => unreachable!(),
            }
            let result = render_models(&args, Ok(discovery)).unwrap();
            assert_eq!(result.exit, ExitCode::Error, "{field}");
            let CmdOutput::Human(text) = result.output else { panic!("JSON text projection"); };
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["ok"], false);
            assert_eq!(json["models"], serde_json::json!([]));
        }
    }

    #[test]
    fn cursor_probe_failure_has_no_legacy_or_fixed_model_fallback() {
        let args = ModelsArgs { provider: "cursor".into(), search: None, json: true };
        for error in [
            DiscoveryError::Executable("cursor-agent"),
            DiscoveryError::CursorProbe(cursor::parse_version(b"Cursor release unknown").unwrap_err()),
        ] {
            let result = render_models(&args, Err(error)).unwrap();
            assert_eq!(result.exit, ExitCode::Error);
            let CmdOutput::Human(text) = result.output else { panic!("JSON text projection"); };
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["provider"], "cursor");
            assert_eq!(json["models"], serde_json::json!([]));
            assert_eq!(json["ok"], false);
        }
    }

    #[test]
    fn native_discovery_failure_has_no_catalog_or_fallback() {
        let args = ModelsArgs {
            provider: "kiro".into(),
            search: None,
            json: true,
        };
        let result = render_models(&args, Err(DiscoveryError::Executable("kiro-cli"))).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Human(text) = result.output else {
            panic!("JSON text projection");
        };
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["models"], serde_json::json!([]));
        assert!(json["error"].as_str().unwrap().contains("kiro-cli"));
    }
}
