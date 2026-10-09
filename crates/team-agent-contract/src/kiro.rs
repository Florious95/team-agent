//! Kiro 2.28.0 adapter boundary. Helper version and chat parameter syntax are
//! observed, including an authenticated model catalog and the V2 R0 surface.
//! Configuration intent and server replies never imply native MCP consumption.
mod catalog;
pub mod interaction;
pub mod native;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::contract::{descriptor::*, hooks::*, plan::*, types::*};

pub const NATIVE_UNVERIFIED: Reason = Reason {
    code: "kiro-r0-missing",
    message: "The requested Kiro capability has no scoped native admission; no fallback is allowed",
};
const SNAPSHOT_UNSUPPORTED: Reason = Reason {
    code: "kiro-no-safe-snapshot",
    message: "No verified safe local full-snapshot or native new-seat API; never copy the account database",
};
const AUTH_UNSUPPORTED: Reason = Reason {
    code: "kiro-native-subscription-only",
    message: "API and compatible-API authentication mappings are not part of this adapter",
};

/// Fifteen facets, with runtime-sensitive facts explicitly Unverified. Supported
/// never means that a documentation example constitutes native acceptance.
pub static KIRO_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet {
        id: "kiro",
        display_name: "Kiro CLI",
        binary: "kiro-cli",
        aliases: &[],
        leader: Support::Unsupported(NATIVE_UNVERIFIED),
    },
    model: ModelFacet {
        selection: Support::Supported(ModelSelection::ExactCatalogId),
        omitted: OmittedModel::Reject,
        catalog: Support::Supported(native::CHAT_CATALOG_SOURCE),
    },
    effort: EffortFacet {
        admission: [
            EffortAdmission::Pass,
            EffortAdmission::Pass,
            EffortAdmission::Pass,
            EffortAdmission::Pass,
            EffortAdmission::Pass,
            EffortAdmission::Reject(Reason {
                code: "kiro-effort-ultra-unmapped",
                message: "Native chat help lists low, medium, high, xhigh and max; not ultra",
            }),
        ],
        carrier: Support::Supported(ValueCarrier::Flag("--effort")),
        inherit_team_default: false,
        // Pass the explicitly requested native flag; the catalog has no model
        // effort schema. This is argv fidelity, not a reasoning-quality claim.
        model_dependent: false,
    },
    auth: AuthFacet {
        subscription: Support::Supported(AuthMechanism::NativeExistingSession),
        official_api: Support::Unsupported(AUTH_UNSUPPORTED),
        compatible_api: Support::Unsupported(AUTH_UNSUPPORTED),
    },
    bypass: BypassFacet {
        intent: Support::Supported(BypassPolicy {
            enabled_arguments: &["--trust-all-tools"],
            requires_startup_consent: true,
        }),
    },
    prompt: PromptFacet {
        carrier: Support::Supported(PromptCarrier::ConfigField),
    },
    mcp: McpFacet {
        carrier: Support::Supported(McpCarrier::ScopedConfig),
        scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS,
        excludes_ambient_configuration: true,
    },
    tool_names: ToolNameFacet {
        naming: Support::Supported(ToolNaming::StaticServer { key: "team" }),
    },
    session: SessionFacet {
        fresh: FreshSession::CaptureAfterLaunch,
        resume: Support::Unverified(NATIVE_UNVERIFIED),
    },
    fork: ForkFacet {
        in_window: Support::Unverified(NATIVE_UNVERIFIED),
        full_snapshot: Support::Unsupported(SNAPSHOT_UNSUPPORTED),
        native_new_seat: Support::Unsupported(SNAPSHOT_UNSUPPORTED),
        allowed_auth: &[AuthMode::NativeSubscription],
    },
    input: InputFacet {
        profiles: Support::Supported(interaction::PROFILES),
    },
    startup: StartupFacet {
        interactive: true,
        authorized_consent: Support::Unverified(NATIVE_UNVERIFIED),
    },
    probe_sources: ProbeSourcesFacet {
        process: Support::Supported(ProbeSource::Host),
        pane: Support::Supported(ProbeSource::NativeSurface),
        server: Support::Supported(ProbeSource::ServerLifecycle),
        client_binding: Support::Unverified(NATIVE_UNVERIFIED),
        round_trip: Support::Unverified(NATIVE_UNVERIFIED),
        semantic: Support::Supported(ProbeSource::NativeSurface),
    },
    workspace: WorkspaceFacet {
        config_scope: ResourceScope::WorkingDirectory,
        shared_cwd: true,
        requires_materialization_lease: true,
    },
    teardown: TeardownFacet {
        resources: &[
            ResourcePolicy {
                kind: ResourceKind::AgentConfig,
                disposition: ResourceDisposition::OwnedRemovable,
            },
            ResourcePolicy {
                kind: ResourceKind::SessionBacking,
                disposition: ResourceDisposition::OwnedPreserved,
            },
            ResourcePolicy {
                kind: ResourceKind::GlobalSettings,
                disposition: ResourceDisposition::Forbidden,
            },
            ResourcePolicy {
                kind: ResourceKind::NativeDatabase,
                disposition: ResourceDisposition::Forbidden,
            },
        ],
    },
};

/// Framework-supplied stdio invocation. No guessed future CLI syntax or ambient
/// environment reads. No Debug: environment values may carry private data.
pub struct McpStdio {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

pub struct KiroAdapter {
    mcp: McpStdio,
}
impl KiroAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "MCP executable")?;
        if mcp.arguments.iter().any(|arg| arg.contains('\0')) {
            return Err(ContractError::Invalid("MCP argv"));
        }
        EnvironmentDelta {
            remove: BTreeSet::new(),
            set: mcp
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), OsString::from(v)))
                .collect(),
        }
        .validate()?;
        Ok(Self { mcp })
    }

    /// Exactly the seven K1 interfaces. Missing native grammars are not replaced
    /// by empty successful implementations, fake parsers or an eighth hook.
    pub fn hooks(&self) -> ProviderHooks<'_> {
        ProviderHooks {
            catalog: HookBinding::Bound(self),
            plan: HookBinding::Bound(self),
            materialize: HookBinding::Bound(self),
            session: HookBinding::Bound(self),
            semantic: HookBinding::Bound(self),
            interaction: HookBinding::Bound(self),
            fork: HookBinding::Unverified(NATIVE_UNVERIFIED),
        }
    }
}

impl PlanHook for KiroAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let request = resolved.request();
        if resolved.provider().as_str() != "kiro" {
            return Err(ContractError::UnknownProvider);
        }
        if request.evidence_kind == EvidenceKind::Native
            && (!interaction::PROFILES[0].matches_native(&request.native) || request.bypass)
        {
            return Err(ContractError::Unverified {
                field: "Kiro native identity or startup consent",
                reason: NATIVE_UNVERIFIED,
            });
        }
        if !native::is_release_version(&request.native.version)
            || request.native.harness != "v2"
            || request.native.ui != "tui"
            || request.channel != Channel::Tmux
            || !matches!(request.operation, Operation::Fresh | Operation::Resume)
            || request.mode != LaunchMode::FullWorker
            || request.auth != AuthMode::NativeSubscription
        {
            return Err(ContractError::Invalid("Kiro V2/TUI launch scope"));
        }
        if request
            .paths
            .executable
            .file_name()
            .is_none_or(|name| name != "kiro-cli-chat")
        {
            return Err(ContractError::Mismatch("Kiro direct chat engine"));
        }
        if self.mcp.executable != request.paths.candidate {
            return Err(ContractError::Mismatch("MCP candidate"));
        }
        let name = owned_agent_name(&request.identity);
        let config = OwnedPath::new(
            request.paths.cwd.path.clone(),
            PathBuf::from(".kiro/agents").join(format!("{name}.json")),
        )?;
        let contents=serde_json::to_vec(&serde_json::json!({
            "name":name,"description":"Team Agent owned worker",
            "prompt":request.prompt.as_ref().ok_or(ContractError::Invalid("worker prompt"))?,
            "tools":["@builtin","@team/send_message","@team/report_result","@team/get_team_status"],
            // Only the three scoped collaboration tools request unattended use.
            // Built-in shell/files stay under native approval unless the caller
            // explicitly requested a separately admitted bypass policy.
            "allowedTools":["@team/send_message","@team/report_result","@team/get_team_status"],
            "includeMcpJson":false,"includePowers":false,
            "mcpServers":{"team":{"command":self.mcp.executable.to_str().ok_or(ContractError::Invalid("MCP path utf8"))?,"args":self.mcp.arguments,"env":self.mcp.environment}}
        })).map_err(|_|ContractError::Invalid("Kiro agent JSON"))?;
        // Native 2.28.0 help confirms that a failed enabled MCP server must
        // stop startup with exit 3, rather than silently dropping the tools.
        let mut arguments: Vec<OsString> = vec![
            "chat".into(),
            "--agent".into(),
            name.into(),
            "--require-mcp-startup".into(),
        ];
        let model = flag(
            &mut arguments,
            "--model",
            resolved
                .model()
                .ok_or(ContractError::Invalid("explicit Kiro native model"))?,
        );
        let effort = match resolved.effort() {
            EffortResolution::Pass(Effort::Ultra) => {
                return Err(ContractError::Unverified {
                    field: "Kiro ultra effort mapping",
                    reason: NATIVE_UNVERIFIED,
                })
            }
            EffortResolution::Pass(effort) => flag(&mut arguments, "--effort", effort.as_str()),
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Ignored { .. } => {
                return Err(ContractError::Invalid("Kiro effort cannot be ignored"))
            }
        };
        let bypass = if request.bypass {
            let index = arguments.len();
            arguments.push("--trust-all-tools".into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index]))
        } else {
            CarrierUse::NotRequested
        };
        if let ExpectedSession::Resume(binding) = resolved.expected_session() {
            arguments.push("--resume-id".into());
            arguments.push(binding.native_session.as_str().into());
        }
        // No positional INPUT, blanket trust, HOME/XDG override, global settings,
        // --resume/picker, catalog substitution, first-message newline or key loop.
        let environment = EnvironmentDelta {
            remove: BTreeSet::from(["KIRO_CHAT_UI".into()]),
            set: BTreeMap::from([("KIRO_CHAT_UI".into(), "tui".into())]),
        };
        Ok(LaunchPlan {
            executable: request.paths.executable.clone(),
            arguments,
            cwd: request.paths.cwd.path.clone(),
            environment,
            expected_session: resolved.expected_session().clone(),
            materialization: vec![OwnedResourceRequest {
                path: config.clone(),
                owner: request.identity.clone(),
                kind: ResourceKind::AgentConfig,
                contents,
            }],
            carriers: CarrierReport {
                model,
                prompt: CarrierUse::Specified(CarrierRef::Resource(config.clone())),
                mcp: CarrierUse::Specified(CarrierRef::Resource(config)),
                bypass,
                effort,
            },
        })
    }
}
fn owned_agent_name(identity: &InstanceIdentity) -> String {
    format!(
        "team-s{}-{}-r{}-{}-i{}-{}-g{}",
        identity.scope.as_str().len(),
        identity.scope.as_str(),
        identity.seat.as_str().len(),
        identity.seat.as_str(),
        identity.instance.as_str().len(),
        identity.instance.as_str(),
        identity.generation.0
    )
}
fn flag(arguments: &mut Vec<OsString>, name: &str, value: &str) -> CarrierUse {
    let index = arguments.len();
    arguments.push(name.into());
    arguments.push(value.into());
    CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1]))
}

impl MaterializeHook for KiroAdapter {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure> {
        let fail = |error, resources| PartialFailure {
            error,
            receipt: MaterializeReceipt { resources },
        };
        if requests.len() != 1 {
            return Err(fail(
                ContractError::Invalid("one owned Kiro agent config required"),
                vec![],
            ));
        }
        let request = &requests[0];
        let config: serde_json::Value = serde_json::from_slice(&request.contents)
            .map_err(|_| fail(ContractError::Invalid("Kiro config JSON"), vec![]))?;
        let expected_name = owned_agent_name(&request.owner);
        let relative = PathBuf::from(".kiro/agents").join(format!("{expected_name}.json"));
        if request.kind != ResourceKind::AgentConfig
            || request.path.relative() != relative
            || config["name"] != expected_name
            || config["includeMcpJson"] != false
            || config["includePowers"] != false
            || !config["prompt"].is_string()
            || config.as_object().is_none_or(|v| {
                v.keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "name"
                            | "description"
                            | "prompt"
                            | "tools"
                            | "allowedTools"
                            | "includeMcpJson"
                            | "includePowers"
                            | "mcpServers"
                    )
                })
            })
            || config["tools"]
                != serde_json::json!([
                    "@builtin",
                    "@team/send_message",
                    "@team/report_result",
                    "@team/get_team_status"
                ])
            || config["allowedTools"]
                != serde_json::json!([
                    "@team/send_message",
                    "@team/report_result",
                    "@team/get_team_status"
                ])
            || config["mcpServers"]
                != serde_json::json!({"team":{"command":self.mcp.executable.to_str(),"args":self.mcp.arguments,"env":self.mcp.environment}})
        {
            return Err(fail(
                ContractError::Invalid("Kiro owned config grant"),
                vec![],
            ));
        }
        let receipt = io.create_exclusive(request)?;
        if receipt.path != request.path
            || receipt.owner != request.owner
            || receipt.kind != request.kind
            || !receipt.exclusive
            || !matches!(receipt.write_effect, ResourceWriteEffect::Written { .. })
        {
            return Err(fail(
                ContractError::Mismatch("Kiro config receipt"),
                vec![receipt],
            ));
        }
        let validation = OwnedValidationRequest {
            resource: receipt.clone(),
            validator_id: "kiro-agent-validate",
            bounds: ReadBounds {
                deadline: Duration::from_secs(15),
                max_output_bytes: 1024 * 1024,
            },
        };
        match io.validate_configuration(&validation) {
            Ok(output)
                if output.exit_code == 0
                    && output.elapsed <= validation.bounds.deadline
                    && output.stdout.len() <= validation.bounds.max_output_bytes =>
            {
                Ok(MaterializeReceipt {
                    resources: vec![receipt],
                })
            }
            _ => Err(fail(
                ContractError::Unverified {
                    field: "Kiro agent validation",
                    reason: NATIVE_UNVERIFIED,
                },
                vec![receipt],
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    CommandSucceeded,
    GeneralFailure,
    McpStartupFailure,
    RequestedAgentNotFound,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitClassification {
    pub class: ExitClass,
    /// Native task/tool effects, distinct from terminal delivery effects.
    pub native_effects_may_have_occurred: bool,
    pub delivery_floor: crate::contract::delivery::DeliveryEffect,
}
/// Official exit-code meanings, not a parser for an unobserved native screen.
/// Code 4 may follow real tool calls. Code 0 is not business-result/MCP readiness.
pub fn classify_exit(
    code: Option<i32>,
    prior: crate::contract::delivery::DeliveryEffect,
) -> ExitClassification {
    ExitClassification {
        class: match code {
            Some(0) => ExitClass::CommandSucceeded,
            Some(1) => ExitClass::GeneralFailure,
            Some(3) => ExitClass::McpStartupFailure,
            Some(4) => ExitClass::RequestedAgentNotFound,
            _ => ExitClass::Unknown,
        },
        native_effects_may_have_occurred: true,
        delivery_floor: prior,
    }
}
