//! The sole new-runtime provider registry. This is not an admission claim:
//! the descriptor's Unverified gates still apply to each requested operation.
use std::collections::BTreeMap;
use std::path::PathBuf;
use team_agent_contract::contract::{
    descriptor::{PromptCarrier, ProviderDescriptor, ToolNaming},
    types::InstanceIdentity,
    hooks::{PlanHook, ProviderHooks},
    plan::{LaunchPlan, ResolvedLaunch},
    types::{ContractError, Support},
};
use team_agent_contract::kiro::{self, KiroAdapter, KIRO_DESCRIPTOR};
use team_agent_contract::providers::codex::{CodexAdapter, CodexLaunchImages, CODEX_DESCRIPTOR};
use team_agent_contract::providers::cursor::{self, CursorAdapter, CURSOR_DESCRIPTOR};
use team_agent_contract::providers::grok_build::{self, GrokAdapter, GROK_BUILD_DESCRIPTOR};
use team_agent_contract::runtime::{
    delivery::{ControlDefinition, StartupDefinition},
    native_panel::{NativePanelPolicy, SessionPanelPolicy},
};

const DESCRIPTORS: &[&ProviderDescriptor] = &[
    &KIRO_DESCRIPTOR,
    &CURSOR_DESCRIPTOR,
    &GROK_BUILD_DESCRIPTOR,
    &CODEX_DESCRIPTOR,
];

#[derive(Clone)]
pub enum NativeRuntime {
    Cursor(cursor::native::RuntimeInstallation),
    GrokBuild(grok_build::native::RuntimeInstallation),
    Codex(CodexLaunchImages),
}

/// Leaf selection stays here; the framework, lifecycle and outbox remain shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeProvider {
    Kiro,
    Cursor,
    GrokBuild,
    Codex,
}

impl NativeProvider {
    pub fn descriptor(self) -> &'static ProviderDescriptor {
        match self {
            Self::Kiro => &KIRO_DESCRIPTOR,
            Self::Cursor => &CURSOR_DESCRIPTOR,
            Self::GrokBuild => &GROK_BUILD_DESCRIPTOR,
            Self::Codex => &CODEX_DESCRIPTOR,
        }
    }

    pub fn action_budget(self) -> std::time::Duration {
        // Kiro's existing budget is unchanged; Cursor's measured cold-ready
        // path needs a larger finite bound, not an unbounded readiness wait.
        // Codex's 30s cap is an unmeasured host limit, not a Codex cold-start measurement.
        std::time::Duration::from_secs(match self {
            Self::Kiro | Self::Codex => 30,
            Self::Cursor | Self::GrokBuild => 90,
        })
    }

    pub fn controls(self) -> &'static [ControlDefinition] {
        match self {
            Self::Kiro => kiro::interaction::CONTROLS,
            Self::Cursor => cursor::interaction::CONTROLS,
            Self::GrokBuild => grok_build::interaction::CONTROLS,
            Self::Codex => &[],
        }
    }

    /// A terminal instruction bridge needs one durable, authorized first packet
    /// before the native client can demonstrate its Team-tool registry.
    pub fn first_business_bootstrap(self) -> bool {
        matches!(self.descriptor().prompt.carrier, Support::Supported(PromptCarrier::RuntimeBridge))
    }

    /// These providers use the separately fenced native tool-roundtrip ledger,
    /// never a guessed neutral tools panel or another provider's inspector.
    pub fn uses_tool_roundtrips(self) -> bool {
        matches!(self, Self::Cursor | Self::GrokBuild)
    }

    pub fn startup(self) -> Option<&'static StartupDefinition> {
        match self {
            Self::GrokBuild => Some(&grok_build::interaction::STARTUP),
            Self::Kiro | Self::Cursor | Self::Codex => None,
        }
    }

    pub fn session_panel(self) -> Option<&'static SessionPanelPolicy> {
        match self {
            Self::GrokBuild => Some(&grok_build::interaction::SESSION_PANEL),
            Self::Kiro | Self::Cursor | Self::Codex => None,
        }
    }

    pub fn tool_panel(self) -> Result<&'static NativePanelPolicy, ContractError> {
        match self {
            Self::Kiro => Ok(&kiro::interaction::TOOL_PANEL),
            Self::Cursor => Err(ContractError::Unverified {
                field: "Cursor native Team-tool inspector",
                reason: cursor::UNVERIFIED,
            }),
            Self::GrokBuild => Err(ContractError::Unverified {
                field: "Grok Build native Team-tool inspector",
                reason: grok_build::UNVERIFIED,
            }),
            Self::Codex => Err(self
                .descriptor()
                .tool_names
                .naming
                .require("codex tool names")
                .expect_err("phase 3 keeps Codex tool names unverified")),
        }
    }

    pub fn server_key_for(self, identity: &InstanceIdentity) -> Result<String, ContractError> {
        if self == Self::GrokBuild {
            return Ok(grok_build::server_key(identity));
        }
        Ok(self.server_key()?.into())
    }

    pub fn server_key(self) -> Result<&'static str, ContractError> {
        match self.descriptor().tool_names.naming.require("tool naming")? {
            ToolNaming::StaticServer { key } => Ok(key),
            ToolNaming::RuntimeBound => Err(ContractError::Invalid("runtime server-key receipt required")),
        }
    }

    pub fn adapter(
        self,
        executable: PathBuf,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<NativeAdapter, ContractError> {
        match self {
            Self::Kiro => KiroAdapter::new(kiro::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::Kiro),
            Self::Cursor => CursorAdapter::new(cursor::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::Cursor),
            Self::GrokBuild => GrokAdapter::new(grok_build::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::GrokBuild),
            Self::Codex => {
                if executable.as_os_str().is_empty() && arguments.is_empty() && environment.is_empty() {
                    Ok(NativeAdapter::Codex(CodexAdapter::new()))
                } else {
                    Err(match CodexAdapter::new().hooks().materialize.require("codex mcp stdio") {
                        Err(err) => err,
                        Ok(_) => panic!("phase 3 keeps Codex H3 unverified"),
                    })
                }
            }
        }
    }
}

pub(super) enum NativeAdapter {
    Kiro(KiroAdapter),
    Cursor(CursorAdapter),
    GrokBuild(GrokAdapter),
    Codex(CodexAdapter),
}

impl NativeAdapter {
    pub fn with_launch_context(
        self,
        project: PathBuf,
        runtime: Option<NativeRuntime>,
    ) -> Result<Self, ContractError> {
        match self {
            Self::Kiro(adapter) if runtime.is_none() => Ok(Self::Kiro(adapter)),
            Self::Kiro(_) => Err(ContractError::Mismatch("native runtime provider")),
            Self::Cursor(adapter) => {
                let adapter = adapter.with_project(project)?;
                Ok(Self::Cursor(match runtime {
                    Some(NativeRuntime::Cursor(runtime)) => adapter.with_runtime(runtime),
                    Some(_) => return Err(ContractError::Mismatch("native runtime provider")),
                    None => adapter,
                }))
            }
            Self::GrokBuild(adapter) => {
                let adapter = adapter.with_project(project)?;
                Ok(Self::GrokBuild(match runtime {
                    Some(NativeRuntime::GrokBuild(runtime)) => adapter.with_runtime(runtime),
                    Some(_) => return Err(ContractError::Mismatch("native runtime provider")),
                    None => adapter,
                }))
            }
            Self::Codex(adapter) => match runtime {
                None => Ok(Self::Codex(adapter)),
                Some(NativeRuntime::Codex(images)) => Ok(Self::Codex(adapter.with_launch_images(images))),
                Some(_) => Err(ContractError::Mismatch("native runtime provider")),
            },
        }
    }

    pub fn hooks(&self) -> ProviderHooks<'_> {
        match self {
            Self::Kiro(adapter) => adapter.hooks(),
            Self::Cursor(adapter) => adapter.hooks(),
            Self::GrokBuild(adapter) => adapter.hooks(),
            Self::Codex(adapter) => adapter.hooks(),
        }
    }

    pub fn plan(&self, request: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        match self {
            Self::Kiro(adapter) => adapter.plan(request),
            Self::Cursor(adapter) => adapter.plan(request),
            Self::GrokBuild(adapter) => adapter.plan(request),
            Self::Codex(adapter) => adapter.plan(request),
        }
    }
}

pub(super) fn provider(key: &str) -> Result<NativeProvider, ContractError> {
    let descriptor = descriptor(key).ok_or(ContractError::Invalid("unregistered contract provider"))?;
    match descriptor.identity.id {
        "kiro" => Ok(NativeProvider::Kiro),
        "cursor" => Ok(NativeProvider::Cursor),
        "grok_build" => Ok(NativeProvider::GrokBuild),
        "codex" => Ok(NativeProvider::Codex),
        _ => Err(ContractError::Invalid("contract provider dispatcher")),
    }
}

pub(crate) fn descriptors() -> &'static [&'static ProviderDescriptor] {
    DESCRIPTORS
}

pub fn descriptor(key: &str) -> Option<&'static ProviderDescriptor> {
    DESCRIPTORS.iter().copied().find(|descriptor| {
        descriptor.identity.id == key || descriptor.identity.aliases.contains(&key)
    })
}

/// Contract dispatch. Codex stays on the legacy path unless the role opted in
/// with the exact boolean `contract_runtime: true`. Other registered providers
/// dispatch regardless of that flag. An unknown key never dispatches.
pub fn contract_dispatch(provider: &str, opt_in: bool) -> bool {
    match descriptor(provider) {
        None => false,
        Some(found) if found.identity.id == "codex" => opt_in,
        Some(_) => true,
    }
}

/// Recognize a misspelled/case-modified contract declaration so it can fail
/// explicitly instead of accidentally selecting a legacy default provider.
pub fn recognizes(key: &str) -> bool {
    let key = key.trim();
    DESCRIPTORS.iter().any(|descriptor| {
        descriptor.identity.id.eq_ignore_ascii_case(key)
            || descriptor
                .identity
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(key))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cursor_does_not_reclassify_legacy_names() {
        assert_eq!(provider("cursor").unwrap(), NativeProvider::Cursor);
        assert_eq!(provider("kiro").unwrap(), NativeProvider::Kiro);
        for legacy in ["cursor_agent", "agent", "pi", "grok"] {
            assert!(descriptor(legacy).is_none());
            assert!(!recognizes(legacy));
            assert!(provider(legacy).is_err());
        }
        assert_eq!(provider("grok_build").unwrap(), NativeProvider::GrokBuild);
        assert!(recognizes(" Grok_Build "));
        assert!(descriptor("Grok_Build").is_none());
        assert!(GROK_BUILD_DESCRIPTOR.identity.aliases.is_empty());
        assert!(recognizes(" Cursor "));
        assert!(descriptor("Cursor").is_none());
        assert_eq!(
            descriptors()
                .iter()
                .map(|descriptor| descriptor.identity.id)
                .collect::<Vec<_>>(),
            ["kiro", "cursor", "grok_build", "codex"]
        );
        assert!(std::ptr::eq(
            provider("codex").unwrap().descriptor(),
            &CODEX_DESCRIPTOR
        ));
        assert!(recognizes(" Codex "));
        assert!(descriptor("Codex").is_none());
        assert!(!contract_dispatch("codex", false));
        assert!(contract_dispatch("codex", true));
        assert!(contract_dispatch("cursor", false));
        assert!(!contract_dispatch("grok", false));
        assert!(!contract_dispatch("pi", false));
        let codex = NativeProvider::Codex;
        assert!(codex.controls().is_empty());
        assert!(codex.startup().is_none());
        assert!(codex.session_panel().is_none());
        assert!(!codex.uses_tool_roundtrips());
        assert!(!codex.first_business_bootstrap());
        assert_eq!(codex.action_budget(), std::time::Duration::from_secs(30));
        match codex.tool_panel() {
            Err(ContractError::Unverified { reason, .. }) => {
                assert_eq!(reason.code, "codex-tool-names-not-observed");
            }
            _ => panic!("codex tool panel is not the unverified tool-name gate"),
        }
        match codex.server_key() {
            Err(ContractError::Unverified { reason, .. }) => {
                assert_eq!(reason.code, "codex-tool-names-not-observed");
            }
            other => panic!("codex server key: {other:?}"),
        }
        assert!(matches!(
            codex.adapter(PathBuf::from("codex"), Vec::new(), BTreeMap::new()),
            Err(ContractError::Unverified { reason, .. }) if reason.code == "codex-h3-no-owned-mount"
        ));
        assert!(matches!(
            codex.adapter(PathBuf::new(), vec!["mcp".into()], BTreeMap::new()),
            Err(ContractError::Unverified { reason, .. }) if reason.code == "codex-h3-no-owned-mount"
        ));
        assert!(matches!(
            codex.adapter(
                PathBuf::new(),
                Vec::new(),
                BTreeMap::from([("CODEX_HOME".into(), "unused".into())])
            ),
            Err(ContractError::Unverified { reason, .. }) if reason.code == "codex-h3-no-owned-mount"
        ));
        let native = codex
            .adapter(PathBuf::new(), Vec::new(), BTreeMap::new())
            .expect("empty codex adapter");
        let err = match native {
            NativeAdapter::Codex(adapter) => match adapter.plan_phase1() {
                Err(error) => error,
                Ok(_) => panic!("launch stays closed"),
            },
            _ => panic!("expected the codex unit adapter"),
        };
        match err {
            ContractError::Unverified { reason, .. } => {
                assert_eq!(reason.code, "codex-launch-not-admitted");
            }
            other => panic!("codex plan: {other:?}"),
        }
        for descriptor in descriptors() {
            assert_eq!(provider(descriptor.identity.id).unwrap().descriptor(), *descriptor);
        }
    }

    #[test]
    fn grok_server_key_is_generation_bound_and_has_no_inspector_fallback() {
        use team_agent_contract::contract::types::{Generation, InstanceId, ScopeId, SeatId};
        let mut identity = InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance").unwrap(), generation: Generation(1),
        };
        let first = NativeProvider::GrokBuild.server_key_for(&identity).unwrap();
        assert_eq!(first, grok_build::server_key(&identity));
        identity.generation = Generation(2);
        assert_ne!(first, NativeProvider::GrokBuild.server_key_for(&identity).unwrap());
        assert!(NativeProvider::GrokBuild.server_key().is_err());
        assert!(NativeProvider::GrokBuild.tool_panel().is_err());
        assert!(NativeProvider::GrokBuild.startup().is_some());
        assert!(NativeProvider::GrokBuild.session_panel().is_some());
        assert!(NativeProvider::GrokBuild.uses_tool_roundtrips());
    }

    #[test]
    fn cursor_has_no_kiro_control_or_tool_inspector_fallback() {
        assert!(NativeProvider::Cursor.controls().is_empty());
        assert!(matches!(NativeProvider::Cursor.tool_panel(), Err(ContractError::Unverified { .. })));
        assert_eq!(NativeProvider::Cursor.server_key().unwrap(), "team");
        assert!(NativeProvider::Cursor.first_business_bootstrap());
        assert!(NativeProvider::Cursor.uses_tool_roundtrips());
        assert!(!NativeProvider::Kiro.first_business_bootstrap());
        assert!(!NativeProvider::Kiro.uses_tool_roundtrips());
        let kiro_panel = NativeProvider::Kiro.tool_panel().unwrap();
        assert_eq!(kiro_panel.server_key, NativeProvider::Kiro.server_key().unwrap());
        assert_eq!(kiro_panel.policy_sha256, kiro::interaction::POLICY);
        assert!(!NativeProvider::Kiro.controls().is_empty());
    }
}
