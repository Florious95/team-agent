//! Bounded native discovery. Nothing here creates a runtime or claims T2/T3.
use std::path::{Path, PathBuf};

use team_agent_contract::contract::{hooks::*, plan::CatalogObservation, types::*};
use team_agent_contract::host::{command::RealCommandRunner, process::resolve_cwd};
use team_agent_contract::kiro::{native::*, KiroAdapter, McpStdio, KIRO_DESCRIPTOR};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("required executable is unavailable: {0}")]
    Executable(&'static str),
    #[error("native discovery failed: {0}")]
    Host(#[from] team_agent_contract::host::HostError),
    #[error("native discovery contract: {0:?}")]
    Contract(ContractError),
    #[error("native model discovery: {0:?}")]
    Catalog(ReadFailure),
    #[error("native discovery I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Discovery {
    pub engine: PathBuf,
    pub native: NativeIdentity,
    pub catalog: CatalogObservation,
}

pub fn executable(name: &'static str) -> Result<PathBuf, DiscoveryError> {
    let path = std::env::var_os("PATH").ok_or(DiscoveryError::Executable(name))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        let Ok(metadata) = std::fs::metadata(&candidate) else { continue; };
        use std::os::unix::fs::PermissionsExt;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok(candidate.canonicalize()?);
        }
    }
    Err(DiscoveryError::Executable(name))
}

pub fn discover(workspace: &Path) -> Result<Discovery, DiscoveryError> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or(DiscoveryError::Executable("HOME for native installation lookup"))?;
    let engine = resolve_helper(&executable(KIRO_DESCRIPTOR.identity.binary)?, &home)?;
    let native = probe_engine(&engine, &mut RealCommandRunner::default(), discovery_bounds())?;
    let request = CatalogRequest {
        provider: ProviderId::new(KIRO_DESCRIPTOR.identity.id).map_err(DiscoveryError::Contract)?,
        native: native.clone(), executable: engine.clone(), cwd: resolve_cwd(workspace)?,
        source: CHAT_CATALOG_SOURCE, bounds: discovery_bounds(),
    };
    let mut host = CatalogReader::new(request.clone(), RealCommandRunner::default()).map_err(DiscoveryError::Contract)?;
    // H1 does not use these pointers; keep the actual current executable and do
    // not manufacture an alternate CLI or execute an MCP server for discovery.
    let adapter = KiroAdapter::new(McpStdio { executable: std::env::current_exe()?.canonicalize()?,
        arguments: Vec::new(), environment: Default::default() }).map_err(DiscoveryError::Contract)?;
    let catalog = adapter.discover(&request, &mut host).map_err(DiscoveryError::Catalog)?;
    Ok(Discovery { engine, native, catalog })
}

pub fn models(args: &crate::cli::ModelsArgs) -> Result<crate::cli::CmdResult, crate::cli::CliError> {
    let result = std::env::current_dir().map_err(DiscoveryError::Io).and_then(|cwd| discover(&cwd));
    render_models(args, result)
}

fn render_models(args: &crate::cli::ModelsArgs, result: Result<Discovery, DiscoveryError>) -> Result<crate::cli::CmdResult, crate::cli::CliError> {
    use crate::cli::{CmdOutput, CmdResult, ExitCode};
    use serde_json::json;
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
                    "version":discovery.native.version,"executable_sha256":team_agent_contract::host::digest_hex(discovery.native.executable_sha256)},
                "current_role_model":null,"search":args.search});
            let mut lines = vec![format!("models.v1 | {} native model IDs:", args.provider)];
            lines.extend(rows.iter().filter_map(|row| row["model_id"].as_str()).map(str::to_owned));
            lines.push("Catalog visibility does not prove subscription authorization, runtime model selection, or effort support.".into());
            (value, lines.join("\n"), ExitCode::Ok)
        }
        Err(error) => {
            let message = error.to_string();
            let action = "Check the selected native executable and existing subscription; no model fallback or login retry was performed.";
            (json!({"schema_version":"models.v1","ok":false,"provider":args.provider,"models":[],"auth":"not_ready","error":message,"action":action}), format!("models.v1 | {message}\n{action}"), ExitCode::Error)
        }
    };
    Ok(CmdResult { output: CmdOutput::Human(if args.json { serde_json::to_string_pretty(&value)? } else { human }), exit, as_json:false, preserve_json_order:true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CmdOutput, ExitCode, ModelsArgs};
    use team_agent_contract::contract::plan::ModelRecord;

    #[test]
    fn native_catalog_projection_preserves_ids_without_claiming_auth_or_effort() {
        let native = NativeIdentity { version:"controlled".into(), harness:"v3".into(), ui:"tui".into(), platform:Platform::Linux, executable_sha256:Digest([1;32]) };
        let discovery = Discovery { engine:"/controlled/kiro-cli-chat".into(), native:native.clone(), catalog:CatalogObservation {
            provider:ProviderId::new("kiro").unwrap(), native, schema:"controlled-json".into(),
            models:vec![ModelRecord { id:"Exact-Case-ID".into(), efforts:Support::Unverified(Reason { code:"not-observed", message:"No effort evidence" }) }],
        }};
        let args = ModelsArgs { provider:"kiro".into(), search:Some("exact".into()), json:true };
        let result = render_models(&args, Ok(discovery)).unwrap();
        assert_eq!(result.exit, ExitCode::Ok);
        let CmdOutput::Human(text) = result.output else { panic!("JSON text projection"); };
        let json:serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["auth"], "unknown");
        assert_eq!(json["models"][0]["model_id"], "Exact-Case-ID");
        assert_eq!(json["models"][0]["efforts"]["state"], "unverified");
        assert_eq!(json["source"]["schema"], "controlled-json");
        assert!(json["current_role_model"].is_null());
    }

    #[test]
    fn native_discovery_failure_has_no_catalog_or_fallback() {
        let args = ModelsArgs { provider:"kiro".into(), search:None, json:true };
        let result = render_models(&args, Err(DiscoveryError::Executable("kiro-cli"))).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        let CmdOutput::Human(text) = result.output else { panic!("JSON text projection"); };
        let json:serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["models"], serde_json::json!([]));
        assert!(json["error"].as_str().unwrap().contains("kiro-cli"));
    }
}
