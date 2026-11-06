//! The cross-runtime boundary cannot silently use another/default Team.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/support/mod.rs"]
mod support;
use support::*;
use team_agent::contract_runtime::forward::FrameworkContext;
use team_agent_contract::contract::types::*;
use team_agent_contract::host::process::resolve_cwd;


#[test]
fn forwarding_route_binds_workspace_team_and_native_scope_without_exposing_payload() {
    let a = Sandbox::new();
    let b = Sandbox::new();
    let mut context = FrameworkContext {
        workspace: resolve_cwd(&a.parent).unwrap(),
        team: "one".into(),
        native_scope: ScopeId::new("native-a").unwrap(),
    };
    let original = context.route().unwrap();
    assert_eq!(context.route().unwrap(), original);
    context.team = "two".into();
    assert_ne!(context.route().unwrap(), original);
    context.team = "one".into();
    context.workspace = resolve_cwd(&b.parent).unwrap();
    assert_ne!(context.route().unwrap(), original);
    context.workspace = resolve_cwd(&a.parent).unwrap();
    context.native_scope = ScopeId::new("native-b").unwrap();
    assert_ne!(context.route().unwrap(), original);
}
