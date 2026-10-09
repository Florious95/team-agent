//! The cross-runtime boundary cannot silently use another/default Team.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/support/mod.rs"]
mod support;
use serde_json::json;
use support::*;
use team_agent::contract_runtime::forward::{drain, FrameworkContext};
use team_agent_contract::contract::types::*;
use team_agent_contract::host::process::resolve_cwd;
use team_agent_contract::orchestration::{forward::*, mcp::*, Error};

#[test]
fn missing_framework_team_refuses_without_claiming_or_creating_a_legacy_runtime() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let native = start(&mut store, &mut FakeHost::default(), &mut FakeIo::new("unused"), "worker").target;
    let framework = FrameworkContext { workspace:resolve_cwd(&sandbox.parent).unwrap(), team:"missing-team".into(), native_scope:store.scope().clone() };
    store.set_framework_routes(&[FrameworkPeer { recipient:"legacy".into(), route:framework.route().unwrap(), provider:"pi".into() }], None).unwrap();
    let caller = CallContext { identity:native.identity, binding_key:native.binding_key, connection_id:InstanceId::new("connection").unwrap(), task_id:"task".into() };
    handle(&mut store, &caller, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"legacy","content":"Task"}}})).unwrap();
    assert_eq!(drain(&framework, &mut store, 10), Err(Error::Fence));
    assert_eq!(store.pending_forwards(10).unwrap().len(), 1);
    assert!(store.deliveries().unwrap().is_empty());
    assert!(!sandbox.parent.join(".team").exists());
}

#[test]
fn forwarding_route_binds_workspace_team_and_native_scope_without_exposing_payload() {
    let a = Sandbox::new();
    let b = Sandbox::new();
    let mut context = FrameworkContext { workspace:resolve_cwd(&a.parent).unwrap(), team:"one".into(), native_scope:ScopeId::new("native-a").unwrap() };
    let original = context.route().unwrap();
    assert_eq!(context.route().unwrap(), original);
    context.team = "two".into(); assert_ne!(context.route().unwrap(), original);
    context.team = "one".into(); context.workspace = resolve_cwd(&b.parent).unwrap(); assert_ne!(context.route().unwrap(), original);
    context.workspace = resolve_cwd(&a.parent).unwrap(); context.native_scope = ScopeId::new("native-b").unwrap(); assert_ne!(context.route().unwrap(), original);
}
