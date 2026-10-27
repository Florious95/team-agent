//! The sole new-runtime provider registry. This is not an admission claim:
//! the descriptor's Unverified gates still apply to each requested operation.
use std::collections::BTreeMap;
use std::path::PathBuf;
use team_agent_contract::contract::{
    descriptor::{PromptCarrier, ProviderDescriptor, ToolNaming},
    types::InstanceIdentity,
    hooks::{PlanHook, ProviderHooks},
    plan::{CatalogObservation, LaunchEntrypoint, LaunchPlan, ResolvedLaunch},
    session::ResumeBinding,
    types::{ContractError, NativeIdentity, Support},
};
use team_agent_contract::kiro::{self, KiroAdapter, KIRO_DESCRIPTOR};
use team_agent_contract::providers::claude::{self, ClaudeAdapter, CLAUDE_DESCRIPTOR};
use team_agent_contract::providers::codex::{self, CodexAdapter, CODEX_DESCRIPTOR};
use team_agent_contract::providers::cursor::{self, CursorAdapter, CURSOR_DESCRIPTOR};
use team_agent_contract::providers::grok_build::{self, GrokAdapter, GROK_BUILD_DESCRIPTOR};
use team_agent_contract::providers::pi::{self, PiAdapter, PI_DESCRIPTOR};
use team_agent_contract::runtime::{
    delivery::{ControlDefinition, StartupDefinition},
    native_panel::{NativePanelPolicy, SessionPanelPolicy},
};

const DESCRIPTORS: &[&ProviderDescriptor] = &[&KIRO_DESCRIPTOR, &CURSOR_DESCRIPTOR, &GROK_BUILD_DESCRIPTOR, &CODEX_DESCRIPTOR, &PI_DESCRIPTOR, &CLAUDE_DESCRIPTOR];
/// Declared keys the legacy runtime also owns (the Codex worker, leader and
/// app-server). Only an explicit `contract_runtime: true` maps one to its leaf,
/// whose distinct canonical id is what compiled specs and state then carry, so
/// legacy readers never mistake a contract seat for a legacy one.
const OPT_IN: &[(&str, &ProviderDescriptor)] = &[("codex", &CODEX_DESCRIPTOR)];
/// Declaration names that select a leaf without opt-in: Cursor's legacy key,
/// its npm package/public command and its binary; Claude Code's and Pi's public
/// names (`pi` is also the default role provider). The leaf's distinct id is
/// what compiled specs and state carry. A persisted legacy state row keeps its
/// key and is never reclassified ([`descriptor`] does not match it).
const TRANSITION: &[(&str, &ProviderDescriptor)] = &[
    ("cursor_agent", &CURSOR_DESCRIPTOR), ("cursor-agent", &CURSOR_DESCRIPTOR), ("agent", &CURSOR_DESCRIPTOR),
    ("claude", &CLAUDE_DESCRIPTOR), ("claude_code", &CLAUDE_DESCRIPTOR), ("claude-code", &CLAUDE_DESCRIPTOR),
    ("pi", &PI_DESCRIPTOR),
];

/// One provider's public command route: the PATH-selected entry the pane execs
/// by bare name and the native image that route must exec into.
#[derive(Clone)]
pub enum NativeRuntime {
    Kiro(kiro::native::RuntimeInstallation),
    Cursor(cursor::native::RuntimeInstallation),
    GrokBuild(grok_build::native::RuntimeInstallation),
    Codex(codex::launcher::RuntimeInstallation),
    Pi(pi::launcher::RuntimeInstallation),
    Claude(claude::launcher::RuntimeInstallation),
}

impl NativeRuntime {
    pub fn launcher(&self) -> LaunchEntrypoint {
        match self {
            Self::Kiro(runtime) => runtime.launcher(),
            Self::Cursor(runtime) => runtime.launcher(),
            Self::GrokBuild(runtime) => runtime.launcher(),
            Self::Codex(runtime) => runtime.launcher(),
            Self::Pi(runtime) => runtime.launcher(),
            Self::Claude(runtime) => runtime.launcher(),
        }
    }

    pub fn engine(&self) -> &PathBuf {
        match self {
            Self::Kiro(runtime) => &runtime.engine,
            Self::Cursor(runtime) => &runtime.engine,
            Self::GrokBuild(runtime) => &runtime.engine,
            Self::Codex(runtime) => &runtime.engine,
            Self::Pi(runtime) => &runtime.engine,
            Self::Claude(runtime) => &runtime.engine,
        }
    }

    pub fn native(&self) -> &NativeIdentity {
        match self {
            Self::Kiro(runtime) => &runtime.native,
            Self::Cursor(runtime) => &runtime.native,
            Self::GrokBuild(runtime) => &runtime.native,
            Self::Codex(runtime) => &runtime.native,
            Self::Pi(runtime) => &runtime.native,
            Self::Claude(runtime) => &runtime.native,
        }
    }

    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        match self {
            Self::Kiro(runtime) => runtime.bind_catalog(catalog),
            Self::Cursor(runtime) => runtime.bind_catalog(catalog),
            Self::GrokBuild(runtime) => runtime.bind_catalog(catalog),
            Self::Codex(runtime) => runtime.bind_catalog(catalog),
            Self::Pi(runtime) => runtime.bind_catalog(catalog),
            Self::Claude(runtime) => runtime.bind_catalog(catalog),
        }
    }
}

/// Leaf selection stays here; the framework, lifecycle and outbox remain shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeProvider {
    Kiro,
    Cursor,
    GrokBuild,
    Codex,
    Pi,
    Claude,
}

impl NativeProvider {
    pub fn descriptor(self) -> &'static ProviderDescriptor {
        match self {
            Self::Kiro => &KIRO_DESCRIPTOR,
            Self::Cursor => &CURSOR_DESCRIPTOR,
            Self::GrokBuild => &GROK_BUILD_DESCRIPTOR,
            Self::Codex => &CODEX_DESCRIPTOR,
            Self::Pi => &PI_DESCRIPTOR,
            Self::Claude => &CLAUDE_DESCRIPTOR,
        }
    }

    pub fn action_budget(self) -> std::time::Duration {
        // Kiro's existing budget is unchanged; Cursor's measured cold-ready
        // path needs a larger finite bound, not an unbounded readiness wait.
        std::time::Duration::from_secs(match self { Self::Kiro => 30, Self::Cursor | Self::GrokBuild | Self::Codex | Self::Pi | Self::Claude => 90 })
    }

    pub fn controls(self) -> &'static [ControlDefinition] {
        match self {
            Self::Kiro => kiro::interaction::CONTROLS,
            Self::Cursor => cursor::interaction::CONTROLS,
            Self::GrokBuild => grok_build::interaction::CONTROLS,
            Self::Codex => codex::interaction::CONTROLS,
            Self::Pi => pi::interaction::CONTROLS,
            Self::Claude => claude::interaction::CONTROLS,
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
        matches!(self, Self::Cursor | Self::GrokBuild | Self::Codex | Self::Pi | Self::Claude)
    }

    pub fn startup(self) -> Option<&'static StartupDefinition> {
        match self {
            Self::GrokBuild => Some(&grok_build::interaction::STARTUP),
            // Claude's folder-trust screen is never answered (focus: No, exit).
            Self::Kiro | Self::Cursor | Self::Codex | Self::Pi | Self::Claude => None,
        }
    }

    pub fn session_panel(self) -> Option<&'static SessionPanelPolicy> {
        match self {
            Self::GrokBuild => Some(&grok_build::interaction::SESSION_PANEL),
            // `/status` and `/session` print history cards, not modals: nothing to close.
            // Claude's fresh session ID is preassigned by argv; nothing to close.
            Self::Kiro | Self::Cursor | Self::Codex | Self::Pi | Self::Claude => None,
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
            Self::Codex => Err(ContractError::Unverified {
                field: "Codex native Team-tool inspector",
                reason: codex::UNVERIFIED,
            }),
            Self::Pi => Err(ContractError::Unverified {
                field: "Pi native Team-tool inspector",
                reason: pi::UNVERIFIED,
            }),
            Self::Claude => Err(ContractError::Unverified {
                field: "Claude Code native Team-tool inspector",
                reason: claude::UNVERIFIED,
            }),
        }
    }

    /// The exact local backing a native new-seat fork reads, for a leaf that
    /// addresses its parent by path (Claude's transcript). None: the native
    /// CLI resolves the parent by ID itself. Pure; the caller checks the file.
    pub fn native_fork_source(
        self,
        runtime: Option<&NativeRuntime>,
        source: &ResumeBinding,
    ) -> Result<Option<PathBuf>, ContractError> {
        match (self, runtime) {
            (Self::Claude, Some(NativeRuntime::Claude(runtime))) => claude::transcript_path(runtime.home(), source).map(Some),
            (Self::Claude, _) => Err(ContractError::Mismatch("native runtime provider")),
            _ => Ok(None),
        }
    }

    pub fn server_key_for(self, identity: &InstanceIdentity) -> Result<String, ContractError> {
        match self {
            Self::GrokBuild => Ok(grok_build::server_key(identity)),
            Self::Codex => Ok(codex::server_key(identity)),
            Self::Pi => Ok(pi::server_key(identity)),
            Self::Claude => Ok(claude::server_key(identity)),
            Self::Kiro | Self::Cursor => Ok(self.server_key()?.into()),
        }
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
            Self::Codex => CodexAdapter::new(codex::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::Codex),
            Self::Pi => PiAdapter::new(pi::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::Pi),
            Self::Claude => ClaudeAdapter::new(claude::McpStdio {
                executable, arguments, environment,
            }).map(NativeAdapter::Claude),
        }
    }
}

pub(super) enum NativeAdapter {
    Kiro(KiroAdapter),
    Cursor(CursorAdapter),
    GrokBuild(GrokAdapter),
    Codex(CodexAdapter),
    Pi(PiAdapter),
    Claude(ClaudeAdapter),
}

impl NativeAdapter {
    pub fn with_launch_context(
        self,
        project: PathBuf,
        runtime: Option<NativeRuntime>,
    ) -> Result<Self, ContractError> {
        match self {
            // Kiro plans from the framework-resolved launch paths alone.
            Self::Kiro(adapter) if matches!(runtime, None | Some(NativeRuntime::Kiro(_))) => Ok(Self::Kiro(adapter)),
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
            Self::Codex(adapter) => {
                let adapter = adapter.with_project(project)?;
                Ok(Self::Codex(match runtime {
                    Some(NativeRuntime::Codex(runtime)) => adapter.with_runtime(runtime),
                    Some(_) => return Err(ContractError::Mismatch("native runtime provider")),
                    None => adapter,
                }))
            }
            Self::Pi(adapter) => {
                let adapter = adapter.with_project(project)?;
                Ok(Self::Pi(match runtime {
                    Some(NativeRuntime::Pi(runtime)) => adapter.with_runtime(runtime),
                    Some(_) => return Err(ContractError::Mismatch("native runtime provider")),
                    None => adapter,
                }))
            }
            Self::Claude(adapter) => {
                let adapter = adapter.with_project(project)?;
                Ok(Self::Claude(match runtime {
                    Some(NativeRuntime::Claude(runtime)) => adapter.with_runtime(runtime),
                    Some(_) => return Err(ContractError::Mismatch("native runtime provider")),
                    None => adapter,
                }))
            }
        }
    }

    pub fn hooks(&self) -> ProviderHooks<'_> {
        match self {
            Self::Kiro(adapter) => adapter.hooks(),
            Self::Cursor(adapter) => adapter.hooks(),
            Self::GrokBuild(adapter) => adapter.hooks(),
            Self::Codex(adapter) => adapter.hooks(),
            Self::Pi(adapter) => adapter.hooks(),
            Self::Claude(adapter) => adapter.hooks(),
        }
    }

    pub fn plan(&self, request: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        match self {
            Self::Kiro(adapter) => adapter.plan(request),
            Self::Cursor(adapter) => adapter.plan(request),
            Self::GrokBuild(adapter) => adapter.plan(request),
            Self::Codex(adapter) => adapter.plan(request),
            Self::Pi(adapter) => adapter.plan(request),
            Self::Claude(adapter) => adapter.plan(request),
        }
    }
}

pub(super) fn provider(key: &str) -> Result<NativeProvider, ContractError> {
    let descriptor = descriptor(key).ok_or(ContractError::Invalid("unregistered contract provider"))?;
    match descriptor.identity.id {
        "kiro" => Ok(NativeProvider::Kiro),
        "cursor" => Ok(NativeProvider::Cursor),
        "grok_build" => Ok(NativeProvider::GrokBuild),
        "codex_tui" => Ok(NativeProvider::Codex),
        "pi_tui" => Ok(NativeProvider::Pi),
        "claude_tui" => Ok(NativeProvider::Claude),
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

/// The leaf a role document selects. Canonical contract keys and transition
/// names (Cursor, Claude Code, Pi) need no flag; the shared legacy `codex` key
/// selects its leaf only with `contract_runtime: true` and is otherwise left to
/// the legacy path unchanged (never re-routed).
pub fn declared(key: &str, contract_runtime: bool) -> Option<&'static ProviderDescriptor> {
    OPT_IN.iter().find(|(shared, _)| contract_runtime && *shared == key)
        .or_else(|| TRANSITION.iter().find(|(retired, _)| *retired == key))
        .map(|(_, descriptor)| *descriptor)
        .or_else(|| descriptor(key))
}

/// The provider name a generated role document records for a leaf. Claude
/// Code and Pi keep their public command names (`claude`, `pi`), which declare
/// the leaf; every other leaf is named by its canonical id.
pub fn public_name(descriptor: &ProviderDescriptor) -> &'static str {
    match descriptor.identity.id {
        "claude_tui" => "claude",
        "pi_tui" => "pi",
        id => id,
    }
}

/// Recognize a misspelled/case-modified contract declaration so it can fail
/// explicitly instead of accidentally selecting a legacy default provider.
/// A shared legacy key is a contract declaration only when opted in.
pub fn recognizes(key: &str, contract_runtime: bool) -> bool {
    let key = key.trim();
    (contract_runtime && OPT_IN.iter().any(|(shared, _)| shared.eq_ignore_ascii_case(key)))
        || TRANSITION.iter().any(|(retired, _)| retired.eq_ignore_ascii_case(key))
        || DESCRIPTORS.iter().any(|descriptor| {
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
        for legacy in ["codex", "grok"] {
            assert!(descriptor(legacy).is_none());
            assert!(!recognizes(legacy, false));
            assert!(provider(legacy).is_err());
        }
        // Every Cursor name declares the leaf; a persisted legacy row keeps its runtime.
        for name in ["cursor", "cursor_agent", "cursor-agent", "agent"] {
            assert_eq!(declared(name, false).unwrap().identity.id, "cursor");
            assert_eq!(declared(name, true).unwrap().identity.id, "cursor");
        }
        for legacy in ["cursor_agent", "cursor-agent", "agent"] {
            assert!(descriptor(legacy).is_none());
            assert!(provider(legacy).is_err());
        }
        assert!(recognizes(" Cursor_Agent ", false));
        assert!(declared("Cursor_Agent", false).is_none());
        assert_eq!(provider("grok_build").unwrap(), NativeProvider::GrokBuild);
        assert!(recognizes(" Grok_Build ", false));
        assert!(descriptor("Grok_Build").is_none());
        assert!(GROK_BUILD_DESCRIPTOR.identity.aliases.is_empty());
        assert!(recognizes(" Cursor ", false));
        assert!(descriptor("Cursor").is_none());
        for descriptor in descriptors() {
            assert_eq!(provider(descriptor.identity.id).unwrap().descriptor(), *descriptor);
        }
    }

    #[test]
    fn bare_codex_keeps_its_legacy_meaning_and_only_opt_in_selects_the_leaf() {
        assert_eq!(CODEX_DESCRIPTOR.identity.id, "codex_tui");
        assert_eq!(CODEX_DESCRIPTOR.identity.binary, "codex");
        assert_eq!(provider("codex_tui").unwrap(), NativeProvider::Codex);
        // Legacy state rows, compiled specs and CLI overrides keep `codex`.
        assert!(descriptor("codex").is_none());
        assert!(provider("codex").is_err());
        assert!(declared("codex", false).is_none());
        assert_eq!(declared("codex", true), Some(&CODEX_DESCRIPTOR));
        assert_eq!(declared("codex_tui", false), Some(&CODEX_DESCRIPTOR));
        for spelling in ["codex", " Codex ", "CODEX"] {
            assert!(!recognizes(spelling, false), "{spelling:?}");
            assert!(recognizes(spelling, true), "{spelling:?}");
        }
        // A case variant opted in is recognized (to fail explicitly), never selected.
        assert!(declared("Codex", true).is_none());
        assert_eq!(declared("claude", true), Some(&CLAUDE_DESCRIPTOR));
        assert_eq!(declared("grok_build", true), Some(&GROK_BUILD_DESCRIPTOR));
        let mut identity = InstanceIdentity {
            scope: team_agent_contract::contract::types::ScopeId::new("scope").unwrap(),
            seat: team_agent_contract::contract::types::SeatId::new("worker").unwrap(),
            instance: team_agent_contract::contract::types::InstanceId::new("instance").unwrap(),
            generation: team_agent_contract::contract::types::Generation(1),
        };
        let first = NativeProvider::Codex.server_key_for(&identity).unwrap();
        assert_eq!(first, codex::server_key(&identity));
        assert_ne!(first, grok_build::server_key(&identity));
        identity.generation = team_agent_contract::contract::types::Generation(2);
        assert_ne!(first, NativeProvider::Codex.server_key_for(&identity).unwrap());
        assert!(NativeProvider::Codex.tool_panel().is_err());
        assert!(NativeProvider::Codex.startup().is_none());
        assert!(NativeProvider::Codex.session_panel().is_none());
        assert!(NativeProvider::Codex.uses_tool_roundtrips());
        assert!(NativeProvider::Codex.first_business_bootstrap());
        let kinds: Vec<_> = NativeProvider::Codex.controls().iter().map(|control| control.command).collect();
        assert_eq!(kinds, ["/status", "/exit"]);
    }

    #[test]
    fn claude_leaf_has_a_distinct_key_and_no_startup_or_panel_fallback() {
        use team_agent_contract::contract::types::{Generation, InstanceId, ScopeId, SeatId};
        assert_eq!(CLAUDE_DESCRIPTOR.identity.id, "claude_tui");
        assert_eq!(CLAUDE_DESCRIPTOR.identity.binary, "claude");
        assert_eq!(provider("claude_tui").unwrap(), NativeProvider::Claude);
        assert_eq!(declared("claude_tui", false), Some(&CLAUDE_DESCRIPTOR));
        // Every public Claude name declares the leaf, with or without opt-in;
        // a persisted legacy row keeps its key and is never reclassified.
        for name in ["claude", "claude_code", "claude-code"] {
            assert_eq!(declared(name, false), Some(&CLAUDE_DESCRIPTOR), "{name}");
            assert_eq!(declared(name, true), Some(&CLAUDE_DESCRIPTOR), "{name}");
            assert!(recognizes(name, false), "{name}");
            assert!(descriptor(name).is_none(), "{name}");
            assert!(provider(name).is_err(), "{name}");
        }
        assert!(declared("Claude", false).is_none());
        assert_eq!(public_name(&CLAUDE_DESCRIPTOR), "claude");
        assert_eq!(declared(public_name(&CLAUDE_DESCRIPTOR), false), Some(&CLAUDE_DESCRIPTOR));
        assert!(recognizes(" Claude ", false));
        assert!(recognizes(" Claude_TUI ", false));
        let mut identity = InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance").unwrap(), generation: Generation(1),
        };
        let first = NativeProvider::Claude.server_key_for(&identity).unwrap();
        assert_eq!(first, claude::server_key(&identity));
        assert_ne!(first, grok_build::server_key(&identity));
        assert_ne!(first, codex::server_key(&identity));
        assert_ne!(first, pi::server_key(&identity));
        identity.generation = Generation(2);
        assert_ne!(first, NativeProvider::Claude.server_key_for(&identity).unwrap());
        assert!(NativeProvider::Claude.server_key().is_err());
        assert!(NativeProvider::Claude.tool_panel().is_err());
        assert!(NativeProvider::Claude.startup().is_none(), "folder trust is never keyed");
        assert!(NativeProvider::Claude.session_panel().is_none());
        assert!(NativeProvider::Claude.uses_tool_roundtrips());
        assert!(NativeProvider::Claude.first_business_bootstrap());
        let kinds: Vec<_> = NativeProvider::Claude.controls().iter().map(|control| control.command).collect();
        assert_eq!(kinds, ["/exit"]);
    }

    #[test]
    fn bare_pi_selects_the_leaf_and_a_legacy_row_is_never_reclassified() {
        use team_agent_contract::contract::types::{Generation, InstanceId, ScopeId, SeatId};
        assert_eq!(PI_DESCRIPTOR.identity.id, "pi_tui");
        assert_eq!(PI_DESCRIPTOR.identity.binary, "pi");
        assert_eq!(provider("pi_tui").unwrap(), NativeProvider::Pi);
        // `pi` (also the default role provider) declares the leaf with or
        // without opt-in; a persisted legacy `pi` row is never reclassified.
        assert!(descriptor("pi").is_none());
        assert!(provider("pi").is_err());
        assert_eq!(declared("pi", false), Some(&PI_DESCRIPTOR));
        assert_eq!(declared("pi", true), Some(&PI_DESCRIPTOR));
        assert_eq!(declared("pi_tui", false), Some(&PI_DESCRIPTOR));
        for spelling in ["pi", " Pi ", "PI"] {
            assert!(recognizes(spelling, false), "{spelling:?}");
            assert!(recognizes(spelling, true), "{spelling:?}");
        }
        assert!(declared("Pi", true).is_none());
        assert_eq!(public_name(&PI_DESCRIPTOR), "pi");
        assert_eq!(public_name(&CURSOR_DESCRIPTOR), "cursor");
        let mut identity = InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance").unwrap(), generation: Generation(1),
        };
        let first = NativeProvider::Pi.server_key_for(&identity).unwrap();
        assert_eq!(first, pi::server_key(&identity));
        assert_ne!(first, codex::server_key(&identity));
        // Pi rejects a tool name over 64 characters.
        assert!(format!("mcp__{first}__report_result").len() <= 64);
        identity.generation = Generation(2);
        assert_ne!(first, NativeProvider::Pi.server_key_for(&identity).unwrap());
        assert!(NativeProvider::Pi.tool_panel().is_err());
        assert!(NativeProvider::Pi.startup().is_none());
        assert!(NativeProvider::Pi.session_panel().is_none());
        assert!(NativeProvider::Pi.uses_tool_roundtrips());
        assert!(NativeProvider::Pi.first_business_bootstrap());
        let kinds: Vec<_> = NativeProvider::Pi.controls().iter().map(|control| control.command).collect();
        assert_eq!(kinds, ["/session", "/quit"]);
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
