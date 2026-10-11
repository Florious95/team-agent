//! The sole new-runtime provider registry. This is not an admission claim:
//! the descriptor's Unverified gates still apply to each requested operation.
use std::collections::BTreeMap;
use std::path::PathBuf;
use team_agent_contract::contract::{
    descriptor::{ProviderDescriptor, ToolNaming},
    hooks::{PlanHook, ProviderHooks},
    plan::{LaunchPlan, ResolvedLaunch},
    types::ContractError,
};
use team_agent_contract::kiro::{self, KiroAdapter, KIRO_DESCRIPTOR};
use team_agent_contract::providers::cursor::{self, CursorAdapter, CURSOR_DESCRIPTOR};
use team_agent_contract::runtime::{delivery::ControlDefinition, native_panel::NativePanelPolicy};

const DESCRIPTORS: &[&ProviderDescriptor] = &[&KIRO_DESCRIPTOR, &CURSOR_DESCRIPTOR];

/// Leaf selection stays here; the framework, lifecycle and outbox remain shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeProvider {
    Kiro,
    Cursor,
}

impl NativeProvider {
    pub fn descriptor(self) -> &'static ProviderDescriptor {
        match self {
            Self::Kiro => &KIRO_DESCRIPTOR,
            Self::Cursor => &CURSOR_DESCRIPTOR,
        }
    }

    pub fn controls(self) -> &'static [ControlDefinition] {
        match self {
            Self::Kiro => kiro::interaction::CONTROLS,
            Self::Cursor => cursor::interaction::CONTROLS,
        }
    }

    pub fn tool_panel(self) -> Result<&'static NativePanelPolicy, ContractError> {
        match self {
            Self::Kiro => Ok(&kiro::interaction::TOOL_PANEL),
            Self::Cursor => Err(ContractError::Unverified {
                field: "Cursor native Team-tool inspector",
                reason: cursor::UNVERIFIED,
            }),
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
        }
    }
}

pub(super) enum NativeAdapter {
    Kiro(KiroAdapter),
    Cursor(CursorAdapter),
}

impl NativeAdapter {
    pub fn hooks(&self) -> ProviderHooks<'_> {
        match self {
            Self::Kiro(adapter) => adapter.hooks(),
            Self::Cursor(adapter) => adapter.hooks(),
        }
    }

    pub fn plan(&self, request: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        match self {
            Self::Kiro(adapter) => adapter.plan(request),
            Self::Cursor(adapter) => adapter.plan(request),
        }
    }
}

pub(super) fn provider(key: &str) -> Result<NativeProvider, ContractError> {
    let descriptor = descriptor(key).ok_or(ContractError::Invalid("unregistered contract provider"))?;
    match descriptor.identity.id {
        "kiro" => Ok(NativeProvider::Kiro),
        "cursor" => Ok(NativeProvider::Cursor),
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
        for legacy in ["cursor_agent", "agent", "pi", "codex"] {
            assert!(descriptor(legacy).is_none());
            assert!(!recognizes(legacy));
            assert!(provider(legacy).is_err());
        }
        assert!(recognizes(" Cursor "));
        assert!(descriptor("Cursor").is_none());
        for descriptor in descriptors() {
            assert_eq!(provider(descriptor.identity.id).unwrap().descriptor(), *descriptor);
        }
    }

    #[test]
    fn cursor_has_no_kiro_control_or_tool_inspector_fallback() {
        assert!(NativeProvider::Cursor.controls().is_empty());
        assert!(matches!(NativeProvider::Cursor.tool_panel(), Err(ContractError::Unverified { .. })));
        assert!(matches!(NativeProvider::Cursor.server_key(), Err(ContractError::Unverified { .. })));
        let kiro_panel = NativeProvider::Kiro.tool_panel().unwrap();
        assert_eq!(kiro_panel.server_key, NativeProvider::Kiro.server_key().unwrap());
        assert_eq!(kiro_panel.policy_sha256, kiro::interaction::POLICY);
        assert!(!NativeProvider::Kiro.controls().is_empty());
    }
}
