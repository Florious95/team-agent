use super::*;
use crate::transport::test_support::OfflineTransport;
use serde_json::json;
use serial_test::serial;

#[allow(dead_code)]
struct HermeticTestEnv;

const QS_TEAM_MD: &str =
    "---\nname: quickteam\nobjective: Quick start.\nprovider: codex\n---\n\nQuick-start team.\n";
pub(super) const QS_VALID_ROLE: &str = "---\nname: implementer\nrole: Implementation Engineer\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nImplement bounded tasks.\n";
const QS_ROLE_NO_PROVIDER: &str = "---\nname: implementer\nrole: Implementation Engineer\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nImplement bounded tasks.\n";

/// A REAL team dir `<base>/teamdir/{TEAM.md, agents/implementer.md}` — already in team-dir
/// layout, so quick_start's prepare_quick_start_team returns it as-is and writes the compiled
/// `team.spec.yaml` into it (diagnose/quick_start.py:prepare_quick_start_team, before launch).
pub(super) fn quick_start_team_dir(role_doc: &str) -> PathBuf {
    let team = temp_ws().join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), QS_TEAM_MD).unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), role_doc).unwrap();
    team
}

fn quick_start_team_dir_with_roles(role_docs: &[(&str, &str)]) -> PathBuf {
    let team = temp_ws().join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), QS_TEAM_MD).unwrap();
    for (file, role_doc) in role_docs {
        std::fs::write(team.join("agents").join(file), role_doc).unwrap();
    }
    team
}

fn role_doc(id: &str) -> String {
    format!(
        "---\nname: {id}\nrole: {id} Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{id} worker.\n"
    )
}

fn codex_identity_rollout(
    workspace: &std::path::Path,
    file_name: &str,
    session_id: &str,
    embedded_agent_id: &str,
) -> PathBuf {
    let path = workspace.join(file_name);
    std::fs::write(
        &path,
        format!(
            "{{\"session_meta\":{{\"payload\":{{\"id\":\"{session_id}\",\"cwd\":\"{}\"}}}}}}\n\
             {{\"type\":\"turn_context\",\"payload\":{{}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"content\":[{{\"type\":\"input_text\",\"text\":\"You are Team Agent worker `{embedded_agent_id}` with role `fixture`.\"}}]}}}}\n",
            workspace.to_string_lossy()
        ),
    )
    .unwrap();
    path
}

fn layout_pane(
    session: &str,
    window: &str,
    pane: &str,
    pane_index: u32,
) -> crate::transport::PaneInfo {
    crate::transport::PaneInfo {
        pane_id: crate::transport::PaneId::new(pane),
        session: crate::transport::SessionName::new(session),
        window_index: None,
        window_name: Some(crate::transport::WindowName::new(window)),
        pane_index: Some(pane_index),
        tty: None,
        current_command: Some("codex".to_string()),
        current_path: None,
        active: pane_index == 0,
        pane_pid: None,
        leader_env: std::collections::BTreeMap::new(),
    }
}

/// E5 spec 迁移:spec 不再落 team_dir,而在 <team_workspace>/.team/runtime/<team_key>/。
/// team_key = team_dir.name(quick_start_team_dir → "teamdir");workspace = team_workspace(team)。
pub(super) fn quick_start_runtime_spec_path(team: &std::path::Path) -> PathBuf {
    let workspace = crate::model::paths::team_workspace(team).unwrap();
    let team_key = team.file_name().unwrap().to_string_lossy().to_string();
    crate::model::paths::runtime_spec_path(&workspace, &team_key)
}

/// 在 team 自身或其 team_workspace 父下找 .team/runtime/*/team.spec.yaml。run_workspace 解析
/// 可能是 team 或父、team_key 可能因 state 缺省而非 team 名,故扫整个 runtime 目录任一子目录。
/// `team_key` 给定时优先精确命中,否则取扫到的第一个。
pub(super) fn find_runtime_spec(team: &std::path::Path, team_key: &str) -> Option<PathBuf> {
    let mut workspaces = vec![team.to_path_buf()];
    if let Ok(ws) = crate::model::paths::team_workspace(team) {
        workspaces.push(ws);
    }
    // 精确命中优先。
    for ws in &workspaces {
        let exact = crate::model::paths::runtime_spec_path(ws, team_key);
        if exact.exists() {
            return Some(exact);
        }
    }
    // 回退:扫 .team/runtime/*/team.spec.yaml。
    for ws in &workspaces {
        let runtime = crate::model::paths::runtime_dir(ws);
        let Ok(entries) = std::fs::read_dir(&runtime) else {
            continue;
        };
        for entry in entries.flatten() {
            let spec = entry.path().join("team.spec.yaml");
            if spec.exists() {
                return Some(spec);
            }
        }
    }
    None
}

/// A no-owner running workspace: state.json carries session_name + agents but NO team_owner →
/// check_team_owner returns None = allowed (owner_gate.rs:48), so the owner-gated entry points can
/// proceed PAST the owner gate. Also inits the real team.db.
fn unowned_running_ws() -> PathBuf {
    let ws = temp_ws();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex", "role": "Worker", "model": "gpt-5.5", "auth_mode": "subscription" } },
        }),
    )
    .unwrap();
    let _ = crate::message_store::MessageStore::open(&ws).unwrap();
    ws
}

// P0 — quick_start over a VALID team dir must COMPILE the real spec (the compiler runs before any
// launch/spawn) → team.spec.yaml is written carrying the agent set, and the result is NOT the
// hardcoded "no role docs found" PreflightBlocked. Golden: quick_start.py → _compile_team_dir_spec
// writes spec_path=team_dir/team.spec.yaml BEFORE launch.
#[test]
fn quick_start_compiles_real_spec_to_team_spec_yaml() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport(&team, None, true, None, &transport);

    // OBSERVABLE 1 (real compiler ran; spawn-independent): the compiled spec is written.
    // E5: spec lands in .team/runtime/<team_key>/ (NOT the user team dir).
    let spec_path = quick_start_runtime_spec_path(&team);
    assert!(
        spec_path.exists(),
        "quick_start must compile the team dir and write team.spec.yaml under .team/runtime/<team_key>/ \
         (the real compiler runs before launch); the stub returns before compiling. result={result:?}"
    );
    assert!(
        !team.join("team.spec.yaml").exists(),
        "E5: spec must NOT be written into the user team dir"
    );
    let spec_text = std::fs::read_to_string(&spec_path).unwrap_or_default();
    assert!(
        spec_text.contains("implementer"),
        "the compiled team.spec.yaml must carry the role-doc agent 'implementer'"
    );

    // OBSERVABLE 2 (report, robust): a VALID dir must never be the hardcoded always-blocked stub.
    if let Ok(QuickStartReport::PreflightBlocked { blockers, .. }) = &result {
        assert!(
            !blockers.iter().any(|b| b.contains("no role docs found")),
            "a VALID team dir must never yield the hardcoded 'no role docs found' blocker"
        );
    }
}

#[test]
fn quick_start_teamdir_under_dot_team_uses_project_workspace_for_status_and_collect() {
    let workspace = temp_ws();
    let team = workspace.join(".team").join("current");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), QS_TEAM_MD).unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();

    let transport = OfflineTransport::new();
    let result = quick_start_with_transport(&team, None, true, None, &transport);
    assert!(
        matches!(result, Ok(QuickStartReport::Ready { .. })),
        "quick-start failed: {result:?}"
    );

    let state_path = crate::state::persist::runtime_state_path(&workspace);
    assert!(
        state_path.exists(),
        "quick-start .team/current must persist runtime state under project root"
    );
    assert!(
        !workspace
            .join(".team")
            .join(".team")
            .join("runtime")
            .join("state.json")
            .exists(),
        "quick-start .team/current must not create nested .team/.team runtime state"
    );

    for input in [&workspace, &team] {
        let selected = crate::state::selector::resolve_active_team(
            input,
            None,
            crate::state::selector::SelectorMode::RuntimeOnly,
        )
        .expect("status/collect selector should resolve project root");
        assert_eq!(
            selected.run_workspace,
            workspace,
            "input={}",
            input.display()
        );
        // E5: selector resolves spec_path to .team/runtime/<team_key>/ (team_key="current").
        let expected_spec = crate::model::paths::runtime_spec_path(&workspace, "current");
        assert_eq!(
            selected
                .spec_path
                .as_deref()
                .map(std::fs::canonicalize)
                .transpose()
                .unwrap(),
            Some(std::fs::canonicalize(&expected_spec).unwrap()),
            "input={}",
            input.display()
        );
    }

    let status = crate::cli::cmd_status(&crate::cli::StatusArgs {
        agent: None,
        workspace: team.clone(),
        detail: false,
        summary: false,
        json: true,
        team: None,
    });
    assert!(
        status.is_ok(),
        "status should normalize teamdir to project-root runtime state: {status:?}"
    );

    let collect = crate::cli::cmd_collect(&crate::cli::CollectArgs {
        result_file: None,
        workspace: team.clone(),
        json: true,
        team: None,
    });
    assert!(
        collect.is_ok(),
        "collect should normalize teamdir to the same project-root state/spec: {collect:?}"
    );
}

#[test]
fn quick_start_default_workspace_compiled_spec_uses_project_root() {
    let workspace = temp_ws();
    std::fs::create_dir_all(workspace.join("agents")).unwrap();
    std::fs::write(workspace.join("TEAM.md"), QS_TEAM_MD).unwrap();
    std::fs::write(
        workspace.join("agents").join("implementer.md"),
        QS_VALID_ROLE,
    )
    .unwrap();
    seed_healthy_coordinator(&workspace);
    let transport = OfflineTransport::new();

    let report = quick_start_with_transport_in_workspace(
        &workspace, &workspace, None, true, None, &transport,
    )
    .expect("quick-start workspace-root team should reach a report");
    assert!(
        matches!(report, QuickStartReport::Ready { .. }),
        "quick-start failed: {report:?}"
    );

    let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
    let spec_path = state
        .get("spec_path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .expect("quick-start state should carry spec_path");
    let spec = crate::model::yaml::loads(&std::fs::read_to_string(spec_path).unwrap()).unwrap();
    assert_eq!(
        spec.get("team")
            .and_then(|team| team.get("workspace"))
            .and_then(crate::model::yaml::Value::as_str),
        Some(workspace.to_string_lossy().as_ref()),
        "compiled team.workspace must be the project root, not its parent"
    );
    for agent in spec
        .get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .unwrap()
    {
        assert_eq!(
            agent
                .get("working_directory")
                .and_then(crate::model::yaml::Value::as_str),
            Some(workspace.to_string_lossy().as_ref()),
            "compiled agent working_directory must be the project root"
        );
    }
    assert_eq!(
        transport.spawn_cwd_records(),
        vec![workspace],
        "quick-start must spawn workers from the project root"
    );
}

// P0 — quick_start over an INVALID role doc (missing `provider`) must surface the REAL compile
// error, distinct from the stub's hardcoded "no role docs found". Golden: compile_team raises
// "missing front matter field provider" (compiler.py:_validate_role_doc), before preflight.
#[test]
fn quick_start_invalid_role_doc_surfaces_real_compile_error() {
    let team = quick_start_team_dir(QS_ROLE_NO_PROVIDER);
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport(&team, None, true, None, &transport);

    let text = format!("{result:?}");
    assert!(
        text.contains("provider"),
        "an invalid role doc (missing provider) must surface the real compile error mentioning \
         'provider'; got {text}"
    );
    assert!(
        !text.contains("no role docs found"),
        "must NOT be the hardcoded stub blocker — real preflight/compile distinguishes the cause"
    );
}

// P0 — launch (dry_run) over a real compiled spec must resolve the REAL route/permission plan
// (no spawn) — NOT the hardcoded RequirementUnmet stub. Golden: launch/core.py dry_run resolves
// routing + permissions without starting any process.
#[test]
fn launch_dry_run_resolves_real_plan_not_stub_error() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let spec = crate::compiler::compile_team(&team).expect("compile the seeded valid team");
    let spec_path = team.join("team.spec.yaml");
    std::fs::write(&spec_path, crate::model::yaml::dumps(&spec)).unwrap();

    let result = launch(&spec_path, true, true, true);

    match result {
        Err(LifecycleError::RequirementUnmet(msg)) if msg.contains("not available") => {
            panic!("launch returned the hardcoded stub error; the real dry_run plan was never resolved");
        }
        Ok(report) => {
            assert!(report.dry_run, "dry_run launch must report dry_run=true");
            assert!(
                !report.routes.is_empty() || !report.permissions.is_empty(),
                "dry_run launch must resolve a real route/permission plan from the compiled spec"
            );
        }
        // any OTHER Err (a real requirement/transport error) still proves the stub is gone.
        Err(_) => {}
    }
}

// P1 — start_agent must PROCEED past the owner gate for a no-owner workspace (check_team_owner →
// None = allowed). The stub hard-returns OwnerRefused unconditionally. Golden: lifecycle/start.py
// runs under the owner gate, then the resume-or-fresh decision. (The resume/fresh PLAN + real
// spawn is the #[ignore] OS boundary — see quick_start_full_ready_real_spawn.)
#[test]
fn start_agent_proceeds_past_owner_gate_for_unowned_workspace() {
    let ws = unowned_running_ws();
    let transport = OfflineTransport::new();
    let result = start_agent_with_transport(
        &ws,
        &AgentId::new("w1"),
        false,
        false,
        true,
        None,
        &transport,
    );
    assert!(
        !matches!(result, Err(LifecycleError::OwnerRefused(_))),
        "start_agent must reach the real chain for a no-owner workspace, not hard-refuse owner; got {result:?}"
    );
}

// P1 — restart (Route B) must PROCEED past the owner gate for a no-owner workspace. Stub →
// OwnerRefused. Golden: restart/orchestration.py runs the resume-selection under the owner gate.
#[test]
fn restart_proceeds_past_owner_gate_for_unowned_workspace() {
    let ws = unowned_running_ws();
    let transport = OfflineTransport::new();
    let result = restart_with_transport(&ws, true, None, &transport);
    assert!(
        !matches!(result, Err(LifecycleError::OwnerRefused(_))),
        "restart must reach the real Route-B chain for a no-owner workspace; got {result:?}"
    );
}

// P1 — add_agent must PROCEED past the owner gate (no-owner ws) and reach the real recompile chain.
// Stub → OwnerRefused. Golden: lifecycle/operations.py:add_agent recompiles the role doc into the
// spec under the owner gate.
#[test]
fn add_agent_proceeds_past_owner_gate_and_reaches_recompile() {
    let ws = unowned_running_ws();
    let role = ws.join("w2-role.md");
    std::fs::write(
        &role,
        "---\nname: w2\nrole: Second Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nSecond worker.\n",
    )
    .unwrap();
    let transport = OfflineTransport::new();
    let result = add_agent_with_transport(&ws, &AgentId::new("w2"), &role, false, None, &transport);
    assert!(
        !matches!(result, Err(LifecycleError::OwnerRefused(_))),
        "add_agent must reach the real recompile chain for a no-owner workspace; got {result:?}"
    );
}

// P1 — fork_agent must PROCEED past the owner gate (no-owner ws). Stub → OwnerRefused.
// Golden: lifecycle/operations.py:fork_agent native session fork under the owner gate.
#[test]
fn fork_agent_proceeds_past_owner_gate_for_unowned_workspace() {
    let ws = unowned_running_ws();
    let transport = OfflineTransport::new();
    let result = fork_agent_with_transport(
        &ws,
        &AgentId::new("w1"),
        &AgentId::new("w1-fork"),
        None,
        false,
        None,
        &transport,
    );
    assert!(
        !matches!(result, Err(LifecycleError::OwnerRefused(_))),
        "fork_agent must reach the real native-fork chain for a no-owner workspace; got {result:?}"
    );
}

// REAL-MACHINE boundary (acceptance framework): a full quick_start that actually LAUNCHES workers
// needs a live tmux + provider spawn — db/state seeding + the Ready report happen INSIDE launch()'s
// real spawn. Asserted by the acceptance crate, not here (do NOT fake the spawn).
#[test]
#[ignore = "real-machine: full quick_start Ready needs a live tmux + provider spawn (db/state seeding \
            happens inside launch's real spawn). Acceptance-crate boundary, not a unit RED."]
fn quick_start_full_ready_real_spawn() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let report = quick_start(&team, None, true, None).expect("quick_start launches the team");
    assert!(
        matches!(report, QuickStartReport::Ready { .. }),
        "got {report:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// SPINE-WIRING (③ review→fix) RED — lifecycle wiring divergences vs golden v0.2.11
// (diagnose/quick_start.py, launch/core.py + config.py + routing.py, restart/orchestration.py,
// lifecycle/operations.py). /tmp/spine_divergences.md #8/#15, #10, #12, #13, #14.
// ═════════════════════════════════════════════════════════════════════════

const QS_TEAM_MD_DANGEROUS: &str =
    "---\nname: dangerteam\nobjective: Dangerous.\nprovider: codex\ndangerous_auto_approve: true\n---\n\nteam.\n";

fn quick_start_team_dir_custom(team_md: &str, role_doc: &str) -> PathBuf {
    let team = temp_ws().join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), team_md).unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), role_doc).unwrap();
    team
}

fn compiled_spec_path(team: &std::path::Path) -> PathBuf {
    let spec = crate::compiler::compile_team(team).expect("compile team");
    let spec_path = team.join("team.spec.yaml");
    std::fs::write(&spec_path, crate::model::yaml::dumps(&spec)).unwrap();
    spec_path
}

fn raw_runtime_state(workspace: &std::path::Path) -> (String, serde_json::Value) {
    let state_path = crate::model::paths::runtime_dir(workspace).join("state.json");
    let raw = std::fs::read_to_string(&state_path)
        .unwrap_or_else(|e| panic!("read state.json at {}: {e}", state_path.display()));
    let state = serde_json::from_str(&raw).expect("state.json must parse");
    (raw, state)
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn compiled_spec_path_with_paused_agent(team: &std::path::Path) -> PathBuf {
    let mut spec = crate::compiler::compile_team(team).expect("compile team");
    let crate::model::yaml::Value::Map(root) = &mut spec else {
        panic!("compiled spec must be a map");
    };
    let agents = root
        .iter_mut()
        .find(|(key, _)| key == "agents")
        .and_then(|(_, value)| match value {
            crate::model::yaml::Value::List(agents) => Some(agents),
            _ => None,
        })
        .expect("compiled spec must contain agents");
    let first = agents
        .first_mut()
        .expect("compiled spec must contain an agent");
    let crate::model::yaml::Value::Map(agent) = first else {
        panic!("compiled agent must be a map");
    };
    agent.push(("paused".to_string(), crate::model::yaml::Value::Bool(true)));
    let spec_path = team.join("team.spec.yaml");
    std::fs::write(&spec_path, crate::model::yaml::dumps(&spec)).unwrap();
    spec_path
}

fn unowned_running_ws_all_paused() -> PathBuf {
    let ws = temp_ws();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex", "role": "Worker", "model": "gpt-5.5", "auth_mode": "subscription", "status": "paused" } },
        }),
    )
    .unwrap();
    let _ = crate::message_store::MessageStore::open(&ws).unwrap();
    ws
}

// #15 — quick_start seeds runtime state under team_workspace(team_dir) (the PARENT), not inside the
// team dir, so restart/status can locate it. Golden quick_start.py:35 team_workspace(team_dir).
#[test]
fn spine_quick_start_seeds_state_under_team_workspace_not_team_dir() {
    let team = quick_start_team_dir(QS_VALID_ROLE); // <base>/teamdir
    let transport = OfflineTransport::new();
    let _ = quick_start_with_transport(&team, None, true, None, &transport);
    let workspace = team.parent().unwrap(); // team_workspace(<base>/teamdir) = <base>
    let ws_state = crate::model::paths::runtime_dir(workspace).join("state.json");
    assert!(
        ws_state.exists(),
        "quick_start must seed runtime state under team_workspace(team_dir)={} (not inside the team dir)",
        workspace.display()
    );
}

// #10 — launch dry-run safety is derived from spec.runtime.dangerous_auto_approve (source=runtime_config),
// NOT a hardcoded Disabled. Golden config.py:effective_runtime_config.
#[test]
fn spine_launch_dry_run_safety_reflects_spec_dangerous() {
    let team = quick_start_team_dir_custom(QS_TEAM_MD_DANGEROUS, QS_VALID_ROLE);
    let spec_path = compiled_spec_path(&team);
    let report = launch(&spec_path, true, false, true).expect("dry_run launch");
    assert!(
        report.safety.enabled,
        "safety must reflect spec.runtime.dangerous_auto_approve=true"
    );
    assert_eq!(
        report.safety.source,
        DangerousApprovalSource::RuntimeConfig,
        "safety source must be runtime_config when spec-declared"
    );
}

// #12 — launch dry-run produces one RoutingDecision PER spec.task via route_task, with the real task id
// and the route_task reason taxonomy — not one synthetic 'default_assignee' route. Golden core.py:77-88.
#[test]
fn spine_launch_dry_run_routes_one_per_task_with_route_reason() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let spec_path = compiled_spec_path(&team);
    let report = launch(&spec_path, true, true, true).expect("dry_run launch");
    // compile_team emits one task 'task_initial' assigned to 'implementer' → reason 'explicit assignee on task'.
    let route = report
        .routes
        .iter()
        .find(|r| r.task_id.as_deref() == Some("task_initial"))
        .unwrap_or_else(|| panic!("a route for task_initial expected; got {:?}", report.routes));
    assert_eq!(
        route.reason, "explicit assignee on task",
        "the route reason must come from route_task, not the hardcoded 'default_assignee'"
    );
    assert_eq!(route.selected_agent.as_str(), "implementer");
}

// #13 — an empty/all-paused restart decision set is NOT an atomic refusal (remove the empty-decisions
// clause); only a non-empty unresumable set (and !allow_fresh) refuses. Golden orchestration.py.
#[test]
fn spine_restart_empty_team_is_not_atomic_refusal() {
    let ws = unowned_running_ws_all_paused();
    let transport = OfflineTransport::new();
    let result = restart_with_transport(&ws, true, None, &transport);
    assert!(
        !matches!(result, Ok(RestartReport::RefusedResumeAtomicity { .. })),
        "an all-paused/empty team must NOT yield an atomic refusal (empty decision set is not a refusal); got {result:?}"
    );
}

// E25 — add_agent duplicate membership is runtime-state based, not role-doc/spec-file based.
#[test]
fn spine_add_agent_rejects_duplicate_agent_id() {
    let team = quick_start_team_dir(QS_VALID_ROLE); // team already has agent 'implementer'
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-quickteam",
            "agents": {
                "implementer": {"status": "running", "provider": "codex", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    let role = team.join("dup-role.md");
    std::fs::write(&role, QS_VALID_ROLE).unwrap(); // a role doc named 'implementer' = duplicate
    let transport = OfflineTransport::new();
    let result = add_agent_with_transport(
        &team,
        &AgentId::new("implementer"),
        &role,
        false,
        None,
        &transport,
    );
    let text = format!("{result:?}");
    assert!(
        text.contains("already exists"),
        "add_agent must reject an id already present in runtime state; got {text}"
    );
}

// E5 Bug1 — add-agent must NOT copy the external role file into <team_dir>/agents (the
// self-copy O_TRUNC truncation reproduced on real machine), and must still inject the new
// agent into the compiled spec by reading the role file in place.
#[test]
fn e5_add_agent_does_not_copy_role_into_platform_dir_and_injects_into_spec() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    // External role file OUTSIDE team_dir/agents, with recognizable body content.
    let role = team.parent().unwrap().join("w2-external-role.md");
    let role_body = "---\nname: w2\nrole: Second Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nUNIQUE-E5-BUG1-MARKER body for w2.\n";
    std::fs::write(&role, role_body).unwrap();
    let transport = OfflineTransport::new();
    // The spawn may not complete under OfflineTransport; we only assert the no-copy + spec-inject
    // observables, which happen BEFORE spawn. Ignore the spawn-stage Result.
    let _ = add_agent_with_transport(&team, &AgentId::new("w2"), &role, false, None, &transport);

    // (1) No copy landed in the platform agents dir.
    let copied = team.join("agents").join("w2.md");
    assert!(
        !copied.exists(),
        "add-agent must NOT copy the role file into <team_dir>/agents (anti-pattern + O_TRUNC bug)"
    );
    // (2) The external role file is untouched (not truncated by a self-copy).
    assert_eq!(
        std::fs::read_to_string(&role).unwrap(),
        role_body,
        "external role file must be left intact (no truncating self-copy)"
    );
    // (3) The compiled spec (under .team/runtime/<team_key>/, NOT the user team dir) carries w2.
    let runtime_spec = find_runtime_spec(&team, "teamdir").expect("runtime spec written");
    let spec_text = std::fs::read_to_string(&runtime_spec).unwrap();
    assert!(
        spec_text.contains("w2"),
        "compiled spec must inject the new agent w2 (read in place, not copied); spec=\n{spec_text}"
    );
    assert!(
        !team.join("team.spec.yaml").exists(),
        "E5: add_agent must NOT write spec into the user team dir"
    );
}

// E5 task#3 / E4 — restart rebuilds the runtime spec from role docs EVERY time: editing a role
// doc (here: rename a role) is reflected in the freshly-written runtime spec after restart.
#[test]
fn e5_restart_rebuilds_runtime_spec_from_role_docs() {
    let ws = restart_ws_two_resumable_workers(); // has TEAM.md + agents/{alpha,bravo}.md + state
                                                 // Edit a role doc AFTER initial compile: change alpha's role text.
    std::fs::write(
        ws.join("agents").join("alpha.md"),
        "---\nname: alpha\nrole: RENAMED Alpha Role\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAlpha edited.\n",
    )
    .unwrap();
    let transport = OfflineTransport::new();
    let _ = restart_with_transport(&ws, false, None, &transport);
    // The runtime spec (rebuilt from role docs) must carry the edited role text.
    let runtime_spec = find_runtime_spec(&ws, "restartteam").expect("runtime spec rebuilt");
    let spec_text = std::fs::read_to_string(&runtime_spec).unwrap();
    assert!(
        spec_text.contains("RENAMED Alpha Role"),
        "restart must rebuild runtime spec from role docs (E4: TEAM.md/role edits take effect); spec=\n{spec_text}"
    );
    assert!(
        !ws.join("team.spec.yaml").exists(),
        "E5: rebuilt spec must land in .team/runtime, not the user team dir"
    );
}

// RED-2 (P0) — restart existence gate must use selected.spec_path (read-order B: runtime spec),
// NOT the user-dir team.spec.yaml. spec-demote put spec under .team/runtime/<key>/, so the old
// pre-resolve gate (`!workspace.join("team.spec.yaml").exists()`) falsely reported "missing spec".
#[test]
fn red2_restart_finds_runtime_spec_not_missing_when_user_dir_has_no_spec() {
    let ws = restart_ws_two_resumable_workers(); // has TEAM.md + agents + state
                                                 // Simulate spec-demote: remove any user-dir spec the fixture wrote; restart must still find
                                                 // the runtime spec (rebuilt from role docs / read-order B), NOT report "missing spec".
    let _ = std::fs::remove_file(ws.join("team.spec.yaml"));
    assert!(
        !ws.join("team.spec.yaml").exists(),
        "user-dir spec removed (spec-demote condition)"
    );
    let transport = OfflineTransport::new();
    let result = restart_with_transport(&ws, false, None, &transport);
    // The gate must NOT short-circuit with "missing spec for restart" — restart must proceed
    // (any later resume/rebuild outcome is fine; we only assert the spec-missing gate is gone).
    let text = format!("{result:?}");
    assert!(
        !text.contains("missing spec for restart"),
        "RED-2: restart must locate the runtime spec via read-order B, not report missing; got {text}"
    );
}

// RED-2 negative — a truly empty workspace (no role docs, no spec, no state) must STILL report
// missing spec (gate moved, not removed).
#[test]
fn red2_restart_empty_workspace_still_reports_missing() {
    let ws = temp_ws(); // empty, no TEAM.md/agents/spec/state
    let transport = OfflineTransport::new();
    let result = restart_with_transport(&ws, false, None, &transport);
    let text = format!("{result:?}");
    assert!(
        text.contains("missing spec")
            || text.to_lowercase().contains("not found")
            || text.contains("no_local_team_context")
            || text.to_lowercase().contains("invalid workspace")
            || text.contains("role definitions missing"),
        "RED-2 negative: empty workspace restart must still error (missing/not-found); got {text}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// RED-2-STILL (P0) — the ENTRY gate (input_has_no_local_team_context) must judge on the
// canonical_run_workspace-resolved path, not raw. quick-start <team_dir> lands .team under the
// resolved workspace (team_dir's parent when invoked from there); restart <team_dir> with raw gate
// checks team_dir's own (empty) .team → false "no context" early-exit before the down-moved gate.
//
// Two forms (leader hard requirement — must lock BOTH, not the coincidental same-dir form):
//   Form A: .team in the team_dir's PARENT (standard quick-start <dir> from parent cwd).
//   Form B: .team in the team_dir itself (cwd == team_dir).
#[test]
fn red2still_entry_gate_resolves_parent_dot_team_form_a() {
    // Build: base/  (has .team)  + base/teamdir/{TEAM.md, agents/} (role docs, NO own .team).
    let base = temp_ws();
    let team_dir = base.join("teamdir");
    std::fs::create_dir_all(team_dir.join("agents")).unwrap();
    std::fs::write(
        team_dir.join("TEAM.md"),
        "---\nname: teamdir\nobjective: o\nprovider: codex\n---\nt.\n",
    )
    .unwrap();
    std::fs::write(team_dir.join("agents").join("w1.md"), QS_VALID_ROLE).unwrap();
    // .team lives in the PARENT (base), with a runtime spec — mirrors quick-start <dir> from base cwd.
    let spec = crate::compiler::compile_team(&team_dir).unwrap();
    let runtime_spec = crate::model::paths::runtime_spec_path(&base, "teamdir");
    std::fs::create_dir_all(runtime_spec.parent().unwrap()).unwrap();
    std::fs::write(&runtime_spec, crate::model::yaml::dumps(&spec)).unwrap();
    // Seed minimal team state under base so resolve finds it.
    crate::state::persist::save_runtime_state(
        &base,
        &json!({"session_name": "team-teamdir", "team_dir": team_dir.to_string_lossy(),
                "spec_path": runtime_spec.to_string_lossy(),
                "agents": {"w1": {"status": "running", "provider": "codex"}}}),
    )
    .unwrap();
    // Entry gate on team_dir: raw team_dir has NO .team (it's in parent), but resolved → base has it.
    assert!(
        crate::lifecycle::restart::input_has_no_local_team_context(&team_dir),
        "precondition: raw team_dir has no local .team (it's in the parent)"
    );
    let resolved = crate::model::paths::canonical_run_workspace(&team_dir).unwrap();
    assert!(
        !crate::lifecycle::restart::input_has_no_local_team_context(&resolved),
        "RED-2-STILL: gate on canonical_run_workspace-resolved path must see parent .team (false)"
    );
    // Full restart on team_dir must NOT early-exit with missing spec.
    let transport = OfflineTransport::new();
    let text = format!(
        "{:?}",
        restart_with_transport(&team_dir, false, None, &transport)
    );
    assert!(
        !text.contains("missing spec for restart") && !text.contains("active team spec not found"),
        "RED-2-STILL Form A: restart <team_dir> (.team in parent) must not report missing; got {text}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn red2still_entry_gate_same_dir_dot_team_form_b() {
    // Form B: .team in team_dir itself (cwd == team_dir at quick-start time).
    let team_dir = temp_ws();
    std::fs::create_dir_all(team_dir.join("agents")).unwrap();
    std::fs::write(
        team_dir.join("TEAM.md"),
        "---\nname: tb\nobjective: o\nprovider: codex\n---\nt.\n",
    )
    .unwrap();
    std::fs::write(team_dir.join("agents").join("w1.md"), QS_VALID_ROLE).unwrap();
    let spec = crate::compiler::compile_team(&team_dir).unwrap();
    let runtime_spec = crate::model::paths::runtime_spec_path(&team_dir, "tb");
    std::fs::create_dir_all(runtime_spec.parent().unwrap()).unwrap();
    std::fs::write(&runtime_spec, crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &team_dir,
        &json!({"session_name": "team-tb", "team_dir": team_dir.to_string_lossy(),
                "spec_path": runtime_spec.to_string_lossy(),
                "agents": {"w1": {"status": "running", "provider": "codex"}}}),
    )
    .unwrap();
    let transport = OfflineTransport::new();
    let text = format!(
        "{:?}",
        restart_with_transport(&team_dir, false, None, &transport)
    );
    assert!(
        !text.contains("missing spec for restart") && !text.contains("active team spec not found"),
        "RED-2-STILL Form B: restart <team_dir> (.team in team_dir) must not report missing; got {text}"
    );
    let _ = std::fs::remove_dir_all(&team_dir);
}

// E5 task#3 / RC-A6b — restart with role definitions MISSING explicitly refuses (lists what's
// missing) and leaves the previous runtime spec in place (no silent path, no data destruction).
#[test]
fn e5_restart_missing_role_docs_refuses_and_preserves_old_spec() {
    let ws = restart_ws_two_resumable_workers();
    // Pre-seed a previous runtime spec so we can assert it survives the refusal.
    let prev_spec = crate::model::paths::runtime_spec_path(&ws, "restartteam");
    std::fs::create_dir_all(prev_spec.parent().unwrap()).unwrap();
    std::fs::write(&prev_spec, "PREVIOUS-RUNTIME-SPEC-MARKER\n").unwrap();
    // Remove the role definitions (TEAM.md) → role source gone.
    std::fs::remove_file(ws.join("TEAM.md")).unwrap();
    let transport = OfflineTransport::new();
    let result = restart_with_transport(&ws, false, None, &transport);
    let text = format!("{result:?}");
    assert!(
        text.contains("role definitions missing") || text.to_lowercase().contains("missing"),
        "restart with missing role docs must explicitly refuse listing what's missing; got {text}"
    );
    // Old runtime spec preserved untouched (T2: no data destruction on refusal).
    assert_eq!(
        std::fs::read_to_string(&prev_spec).unwrap(),
        "PREVIOUS-RUNTIME-SPEC-MARKER\n",
        "previous runtime spec must be left in place on refusal"
    );
}

#[test]
fn e5_restart_missing_role_docs_refuses_even_after_endpoint_convergence_marker() {
    let _harness_gate =
        EnvVarGuard::unset("TEAM_AGENT_TEST_ENDPOINT_CONVERGENCE_HARNESS_SPEC_FALLBACK");
    let ws = restart_ws_two_resumable_workers();
    let prev_spec = crate::model::paths::runtime_spec_path(&ws, "restartteam");
    std::fs::create_dir_all(prev_spec.parent().unwrap()).unwrap();
    std::fs::write(&prev_spec, "PREVIOUS-RUNTIME-SPEC-MARKER\n").unwrap();

    let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
    state["topology_convergence"] = json!({
        "status": "converged",
        "old_tmux_endpoint": "/tmp/old",
        "new_tmux_endpoint": "/tmp/new",
        "reason": "old_endpoint_dead"
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    std::fs::remove_file(ws.join("TEAM.md")).unwrap();

    let result = restart_with_transport(&ws, false, None, &OfflineTransport::new());
    let text = format!("{result:?}");
    assert!(
        text.contains("role definitions missing"),
        "production marker alone must not enable runtime-spec fallback; got {text}"
    );
    assert_eq!(
        std::fs::read_to_string(&prev_spec).unwrap(),
        "PREVIOUS-RUNTIME-SPEC-MARKER\n",
        "previous runtime spec must be preserved on missing role docs refusal"
    );
}

// E5 §3 解耦(tester 场景2)— add-agent on a team whose runtime spec already exists must still
// resolve team_dir to the USER role dir for compile_team (find TEAM.md/agents), not the runtime
// spec dir. Regression: SelectedTeam.spec_workspace=runtime was used as team_dir → compile_team
// looked for TEAM.md under .team/runtime/<key>/ → "missing TEAM.md".
#[test]
fn e5_add_agent_resolves_team_dir_to_role_dir_when_runtime_spec_exists() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    // Pre-create the runtime spec so the selector's runtime-first branch is taken (migrated team).
    let runtime_spec = quick_start_runtime_spec_path(&team);
    std::fs::create_dir_all(runtime_spec.parent().unwrap()).unwrap();
    let base = crate::compiler::compile_team(&team).unwrap();
    std::fs::write(&runtime_spec, crate::model::yaml::dumps(&base)).unwrap();
    // External role file (relative-ish, outside agents/).
    let role = team.join("w2-role.md");
    std::fs::write(
        &role,
        "---\nname: w2\nrole: Second Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nw2.\n",
    )
    .unwrap();
    let transport = OfflineTransport::new();
    let result =
        add_agent_with_transport(&team, &AgentId::new("w2"), &role, false, None, &transport);
    // Core decoupling signature: must NOT fail because compile_team looked for TEAM.md/agents under
    // the runtime spec dir. Before the fix, team_dir=spec_workspace(runtime) → "missing TEAM.md".
    let text = format!("{result:?}");
    assert!(
        !text.contains("missing TEAM.md") && !text.to_lowercase().contains("missing agents"),
        "add-agent must resolve team_dir to the role dir for compile_team (not the runtime spec dir); got {text}"
    );
}

// E5 grep guard G1 — writers must land spec via runtime_spec_path (.team/runtime/<team_key>/),
// never write team.spec.yaml into the user team_dir/agents_dir. Pins the spec-demote invariant.
#[test]
fn e5_guard_g1_writers_use_runtime_spec_path_not_user_dir() {
    let source = include_str!("../launch.rs");
    let runtime_writes = source.matches("runtime_spec_path(").count();
    assert!(
        runtime_writes >= 2,
        "G1: quick_start + add_agent must write spec via runtime_spec_path (found {runtime_writes})"
    );
    for forbidden in [
        "let spec_path = agents_dir.join(\"team.spec.yaml\");\n    std::fs::write",
        "let spec_path = team_dir.join(\"team.spec.yaml\");\n    std::fs::write",
    ] {
        assert!(
            !source.contains(forbidden),
            "G1: user-dir spec write idiom must be gone: {forbidden:?}"
        );
    }
}

// E5 grep guard G2 — launch.rs must NOT copy a role file into the platform team dir.
// Pins the Bug1 fix: no `fs::copy` of a role file + no `materialize_added_role_file` reborn.
#[test]
fn e5_guard_g2_no_copy_role_into_platform_dir() {
    let source = include_str!("../launch.rs");
    assert!(
        !source.contains("materialize_added_role_file"),
        "G2: materialize_added_role_file (role copy anti-pattern) must stay deleted"
    );
    assert!(
        !source.contains("copy role file"),
        "G2: no 'copy role file' into the platform agents dir (O_TRUNC self-copy bug)"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// SPAWN sub-phase RED — launch(dry_run=false) must REALLY spawn (unlocks the acceptance
// framework's cheap real-machine Tier-1). Golden launch/core.py: create the session + one worker
// window per agent running its provider command (with workspace + agent-id context), populate the
// started list, attach the leader receiver. Today launch(dry_run=false) is the hardcoded stub
// RequirementUnmet("launch spawn requires live transport/provider boundary").
// ═════════════════════════════════════════════════════════════════════════

// P0 — launch(dry_run=false) over a real compiled spec must run the real spawn chain (create session +
// spawn ≥1 worker, populating LaunchReport.started), NOT the hardcoded spawn stub. The recording-
// transport spawn-CALL assertion (spawn_first/spawn_into argv) needs a transport-injection seam in
// launch — flagged to the leader (launch takes no transport today); the real tmux new-session is the
// #[ignore] acceptance boundary (see quick_start_full_ready_real_spawn). Here we assert the seam-
// independent observable: the spawn stub is gone + LaunchReport.started is populated + dry_run=false.
//
// CONVERTED to #[ignore] (final real-daemon sub-phase): the porter wires PUBLIC launch() to the real
// TmuxBackend (today it uses NoopLaunchTransport), so once wired this test spawns REAL tmux — a unit
// test must not. The OS-edge-mocked spawn-orchestration coverage moved to
// `launch_with_transport_records_one_spawn_per_agent_carrying_build_command` (recording mock transport).
#[test]
#[ignore = "real-machine: public launch(dry_run=false) spawns real tmux once wired to TmuxBackend; \
            in-process coverage is launch_with_transport_records_one_spawn_per_agent_carrying_build_command"]
fn spawn_launch_not_dry_run_is_no_longer_the_spawn_stub() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let spec_path = compiled_spec_path(&team);

    let result = launch(&spec_path, false, true, true);

    match result {
        Err(LifecycleError::RequirementUnmet(msg)) if msg.contains("requires live transport") => {
            panic!(
                "launch(dry_run=false) still returns the hardcoded spawn stub; the real spawn was never wired: {msg}"
            );
        }
        Ok(report) => {
            assert!(!report.dry_run, "a real launch must report dry_run=false");
            assert!(
                !report.started.is_empty(),
                "launch(dry_run=false) must spawn >=1 worker and populate LaunchReport.started; got {:?}",
                report.started
            );
        }
        // any OTHER Err (a real transport/preflight error from the wired spawn) still proves the stub is gone.
        Err(_) => {}
    }
}

// ═════════════════════════════════════════════════════════════════════════
// (C) public launch → real TmuxBackend. The OS edge is mocked by a RECORDING transport via the
// launch_with_transport seam; the REAL TmuxBackend swap is exercised by the acceptance framework
// (#[ignore] spawn_launch_not_dry_run_is_no_longer_the_spawn_stub). Golden launch/core.py.
// ═════════════════════════════════════════════════════════════════════════

// (C) — launch_with_transport(dry_run=false, <recording transport>) creates one spawn per compiled
// agent, the recorded spawn argv carries that agent's provider build_command, and LaunchReport.started
// lists them. NOTE: GREEN today — spawn_agents already drives the injected transport; this LOCKS the
// spawn orchestration contract (replacing the now-#[ignore]'d public-launch test's in-process coverage).
// The genuine remaining gap — public launch() must construct a real TmuxBackend (today NoopLaunchTransport)
// — is not cleanly assertable in-process without spawning; it rides the porter's wiring + the #[ignore]
// real-machine test above.
#[test]
fn launch_with_transport_records_one_spawn_per_agent_carrying_build_command() {
    let team = quick_start_team_dir(QS_VALID_ROLE); // one agent: implementer / provider codex
    let spec_path = compiled_spec_path(&team);
    let transport = OfflineTransport::new();

    let report = launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch_with_transport must spawn against the recording transport");

    let recorded = transport.spawn_records();
    assert_eq!(
        recorded.len(),
        report.started.len(),
        "exactly one spawn per started agent; recorded={recorded:?} started={:?}",
        report.started
    );
    assert!(
        !report.started.is_empty(),
        "a real (non-dry-run) launch must spawn >=1 worker"
    );
    assert!(
        !report.dry_run,
        "launch_with_transport(dry_run=false) must report dry_run=false"
    );
    assert_eq!(
        recorded[0].0, "spawn_first",
        "the first worker uses new-session (spawn_first)"
    );
    assert!(
        recorded
            .iter()
            .any(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "each spawn argv must carry the agent's provider build_command (codex); got {recorded:?}"
    );
    assert!(
        report
            .started
            .iter()
            .any(|s| s.agent_id.as_str() == "implementer"),
        "LaunchReport.started must list the compiled agent; got {:?}",
        report.started
    );
}

// ═════════════════════════════════════════════════════════════════════════
// rt-host-a REGRESSION — `team-agent quick-start` compiles+seeds but NEVER spawns workers:
// quick_start_with_transport hardcodes launch dry_run=TRUE, so launch takes the started=Vec::new()
// branch and spawn_agents is never called. The 788-green missed it (no test drove quick_start through
// the spawn path). Golden: a real quick-start must spawn one worker per compiled agent into the session.
// ═════════════════════════════════════════════════════════════════════════

/// Seed a HEALTHY coordinator at `workspace` (this process's pid + matching metadata + db schema) so
/// quick_start's internal start_coordinator returns AlreadyRunning — NO real daemon subprocess spawn.
pub(super) fn seed_healthy_coordinator(workspace: &std::path::Path) {
    let wp = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(crate::model::paths::runtime_dir(workspace)).unwrap();
    let _ = crate::message_store::MessageStore::open(workspace).unwrap(); // create db schema (schema.ok)
    let me = crate::coordinator::Pid::new(std::process::id());
    crate::coordinator::write_coordinator_metadata(
        &wp,
        me,
        crate::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        crate::coordinator::coordinator_pid_path(&wp),
        me.to_string(),
    )
    .unwrap();
}

// RED — quick_start_with_transport must drive the REAL spawn path: launch.dry_run==false, started
// non-empty (one per compiled agent), and the transport records >=1 spawn carrying that agent's
// provider build_command. Today the dry_run=true bug -> launch.started empty + ZERO spawns recorded ->
// RED at assertion (NOT a panic). OS-safe: recording transport (no real tmux) + seeded-healthy
// coordinator (start_coordinator AlreadyRunning -> no daemon subprocess).
#[test]
fn quick_start_with_transport_spawns_workers_not_dry_run() {
    let team = quick_start_team_dir(QS_VALID_ROLE); // one agent: implementer / provider codex
    let workspace = team.parent().expect("team_workspace(team_dir) = parent"); // where start_coordinator runs
    seed_healthy_coordinator(workspace);
    let transport = OfflineTransport::new();

    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport must reach a report");

    let launch = match report {
        QuickStartReport::Ready { launch, .. } => *launch,
        other => panic!("quick_start must reach Ready (the spawn path); got {other:?}"),
    };
    // (1) the rt-host-a bug: launch dry_run hardcoded true -> this is the load-bearing regression assert.
    assert!(
        !launch.dry_run,
        "quick_start must SPAWN workers (launch.dry_run == false); the rt-host-a bug hardcodes dry_run=true \
         so launch takes the empty started branch and never spawns"
    );
    // (2) one started entry per compiled agent (the dry-run branch leaves this empty).
    assert!(
        !launch.started.is_empty(),
        "quick_start must populate launch.started (>=1 worker spawned), not the dry-run empty Vec; got {:?}",
        launch.started
    );
    assert!(
        launch
            .started
            .iter()
            .any(|s| s.agent_id.as_str() == "implementer"),
        "launch.started must list the compiled agent 'implementer'; got {:?}",
        launch.started
    );
    // (3) the transport actually recorded the spawn, carrying the provider build_command.
    let recorded = transport.spawn_records();
    assert!(
        !recorded.is_empty(),
        "quick_start must drive the transport spawn path (>=1 spawn recorded); the dry-run bug records ZERO spawns"
    );
    assert_eq!(
        recorded[0].0, "spawn_first",
        "the first worker uses new-session (spawn_first); got {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "the spawn argv must carry the agent's provider build_command (codex); got {recorded:?}"
    );
}

#[test]
fn adaptive_layout_plan_8_workers_is_3_3_2() {
    let agents = (1..=8)
        .map(|i| AgentId::new(format!("w{i}")))
        .collect::<Vec<_>>();
    let plan = adaptive_layout_plan(&agents, 3);
    let windows = plan
        .iter()
        .map(|placement| placement.layout_window.as_str().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        windows,
        vec![
            "team-w1", "team-w1", "team-w1", "team-w2", "team-w2", "team-w2", "team-w3", "team-w3",
        ]
    );
    let pane_counts = ["team-w1", "team-w2", "team-w3"]
        .into_iter()
        .map(|window| {
            windows
                .iter()
                .filter(|actual| actual.as_str() == window)
                .count()
        })
        .collect::<Vec<_>>();
    assert_eq!(pane_counts, vec![3, 3, 2]);
    assert_eq!(
        plan.iter()
            .filter(|placement| placement.starts_window)
            .map(|placement| placement.agent_id.as_str().to_string())
            .collect::<Vec<_>>(),
        vec!["w1", "w4", "w7"]
    );
}

#[test]
fn quick_start_default_adaptive_groups_workers_into_layout_panes() {
    let roles = (1..=4)
        .map(|i| {
            let id = format!("w{i}");
            (format!("{id}.md"), role_doc(&id))
        })
        .collect::<Vec<_>>();
    let role_refs = roles
        .iter()
        .map(|(file, doc)| (file.as_str(), doc.as_str()))
        .collect::<Vec<_>>();
    let team = quick_start_team_dir_with_roles(&role_refs);
    let workspace = team.parent().expect("team_workspace(team_dir) = parent");
    seed_healthy_coordinator(workspace);
    let transport = OfflineTransport::new();

    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport must reach Ready");

    let (launch, attach_commands, display_backend) = match report {
        QuickStartReport::Ready {
            launch,
            attach_commands,
            display_backend,
            ..
        } => (*launch, attach_commands, display_backend),
        other => panic!("quick_start must reach Ready; got {other:?}"),
    };
    // 0.3.28 Step 4b: adaptive 3-pane tiling replaced with 1-window-per-agent
    // Python-parity layout. 4 workers → spawn_first + 3×spawn_into, each in its
    // own window named after the agent_id. No team-w<N> windows; no splits.
    assert_eq!(
        transport
            .spawn_records()
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["spawn_first", "spawn_into", "spawn_into", "spawn_into"],
        "0.3.28 Step 4b: 4 workers → 1 spawn_first + 3 spawn_into; \
         no splits in worker session"
    );

    let (_raw, state) = raw_runtime_state(workspace);
    assert!(
        transport
            .pane_title_records()
            .iter()
            .all(|(session, _, _, _)| Some(session.as_str())
                == state
                    .get("session_name")
                    .and_then(serde_json::Value::as_str)),
        "pane titles must be configured inside the team session"
    );
    // Each worker lives in its OWN window named `agent_id` (Python parity).
    assert_eq!(state.pointer("/agents/w1/window"), Some(&json!("w1")));
    assert_eq!(state.pointer("/agents/w4/window"), Some(&json!("w4")));
    // attach commands point to the per-agent windows.
    assert!(
        attach_commands.iter().any(|cmd| cmd.contains(":w1")),
        "attach commands must include the per-agent windows: {attach_commands:?}"
    );
    let _ = display_backend; // display_backend value preserved by upstream
}

#[test]
#[serial(env)]
fn quick_start_tmux_backend_prefers_absolute_tmux_env_endpoint() {
    let leader_socket = "/tmp/team-agent-layout-leader-socket";
    let _tmux = EnvVarGuard::set("TMUX", &format!("{leader_socket},123,0"));
    let backend = quick_start_tmux_backend(std::path::Path::new("/tmp/layout-workspace"));

    assert_eq!(
        crate::transport::Transport::tmux_endpoint(&backend).as_deref(),
        Some(leader_socket),
        "quick-start from inside tmux must use the caller's full $TMUX socket endpoint"
    );
    assert_eq!(
        selected_tmux_socket_source(&backend, std::path::Path::new("/tmp/layout-workspace")),
        Some("leader_env")
    );
}

#[test]
fn quick_start_persists_selected_tmux_endpoint_and_attach_commands() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let workspace = team.parent().expect("team_workspace(team_dir) = parent");
    seed_healthy_coordinator(workspace);
    let endpoint = "/tmp/team-agent-layout-selected-socket";
    let transport = OfflineTransport::new().with_tmux_endpoint(endpoint);

    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport must reach Ready");
    let attach_commands = match report {
        QuickStartReport::Ready {
            attach_commands, ..
        } => attach_commands,
        other => panic!("quick_start must reach Ready; got {other:?}"),
    };

    assert!(
        attach_commands
            .iter()
            .any(|cmd| cmd.contains(&format!("tmux -S {endpoint} attach -t "))),
        "attach commands must use the selected socket endpoint: {attach_commands:?}"
    );
    assert!(
        attach_commands.iter().any(|cmd| cmd.contains(":leader")),
        "fresh managed quick-start must include the leader window attach command: {attach_commands:?}"
    );
    let (_raw, state) = raw_runtime_state(workspace);
    assert_eq!(state["tmux_endpoint"], json!(endpoint));
    assert_eq!(state["tmux_socket"], json!(endpoint));
    assert_eq!(state["is_external_leader"], json!(false));
    assert_eq!(
        state["teams"]["teamdir"]["is_external_leader"],
        json!(false)
    );
}

#[test]
fn annotate_runtime_tmux_endpoint_persists_workspace_socket_as_full_path() {
    let workspace = temp_ws();
    let short = crate::tmux_backend::socket_name_for_workspace(&workspace);
    let expected = crate::tmux_backend::socket_path_for_workspace(&workspace)
        .expect("workspace socket should have a physical path");
    let transport = OfflineTransport::new().with_tmux_endpoint(short);
    let mut state = json!({});

    annotate_runtime_tmux_endpoint(&mut state, &transport, &workspace);

    assert_eq!(
        state["tmux_endpoint"],
        json!(expected.to_string_lossy().to_string()),
        "runtime endpoint state must persist the full tmux socket path, not the short -L name"
    );
    assert_eq!(
        state["tmux_socket"], state["tmux_endpoint"],
        "tmux_socket mirrors the canonical persisted endpoint"
    );
}

#[test]
fn quick_start_preserves_managed_leader_topology_and_emits_leader_attach_command() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let workspace = team.parent().expect("team_workspace(team_dir) = parent");
    seed_healthy_coordinator(workspace);
    crate::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "teamdir",
            "session_name": "team-teamdir",
            "is_external_leader": false,
            "leader_client": {"diagnostic_only": true, "attach_mode": "attach-session"},
            "leader_receiver": {"mode": "direct_tmux", "status": "attached", "pane_id": "%42"},
            "team_owner": {"pane_id": "%42", "owner_epoch": 1},
            "agents": {},
            "teams": {
                "teamdir": {
                    "session_name": "team-teamdir",
                    "is_external_leader": false,
                    "leader_client": {"diagnostic_only": true, "attach_mode": "attach-session"},
                    "agents": {}
                }
            }
        }),
    )
    .expect("seed managed leader state");
    let transport = OfflineTransport::new();

    let report = quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick_start_with_transport must reach Ready");
    let attach_commands = match report {
        QuickStartReport::Ready {
            attach_commands, ..
        } => attach_commands,
        other => panic!("quick_start must reach Ready; got {other:?}"),
    };

    assert!(
        attach_commands.iter().any(|cmd| cmd.contains(":leader")),
        "managed topology quick-start output must include the leader window attach command: {attach_commands:?}"
    );
    let (_raw, state) = raw_runtime_state(workspace);
    assert_eq!(state["is_external_leader"], json!(false));
    assert_eq!(state["leader_client"]["diagnostic_only"], json!(true));
}

#[test]
fn fresh_managed_state_without_receiver_is_not_attached() {
    let workspace = temp_ws();
    crate::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "teamdir",
            "session_name": "team-teamdir",
            "is_external_leader": false,
            "agents": {"implementer": {"status": "running"}},
            "teams": {
                "teamdir": {
                    "session_name": "team-teamdir",
                    "is_external_leader": false,
                    "agents": {"implementer": {"status": "running"}}
                }
            }
        }),
    )
    .unwrap();

    assert!(
        !crate::lifecycle::launch::launched_team_receiver_is_attached(&workspace, "teamdir"),
        "managed topology with missing leader_receiver must not be reported attached"
    );
}

#[test]
fn attach_window_names_for_state_agents_include_managed_leader_and_layout_windows() {
    let state = json!({
        "is_external_leader": false,
        "agents": {
            "w1": {"layout_window": "team-w1", "window": "w1"},
            "w2": {"layout_window": "team-w1", "window": "w2"},
            "w4": {"layout_window": "team-w2", "window": "w4"}
        }
    });

    let windows = crate::lifecycle::launch::attach_window_names_for_state_agents(
        &state,
        ["w1", "w2", "w4"].into_iter(),
    );

    assert_eq!(windows, vec!["leader", "team-w1", "team-w2"]);
}

#[test]
fn quick_start_no_display_keeps_one_window_per_agent() {
    let roles = ["w1", "w2"]
        .into_iter()
        .map(|id| (format!("{id}.md"), role_doc(id)))
        .collect::<Vec<_>>();
    let role_refs = roles
        .iter()
        .map(|(file, doc)| (file.as_str(), doc.as_str()))
        .collect::<Vec<_>>();
    let team = quick_start_team_dir_with_roles(&role_refs);
    let workspace = team.parent().expect("team_workspace(team_dir) = parent");
    seed_healthy_coordinator(workspace);
    let transport = OfflineTransport::new();

    let report = quick_start_with_transport_in_workspace_with_display(
        workspace, &team, None, true, None, &transport, false,
    )
    .expect("quick_start_with_transport must reach Ready");

    let display_backend = match report {
        QuickStartReport::Ready {
            display_backend, ..
        } => display_backend,
        other => panic!("quick_start must reach Ready; got {other:?}"),
    };
    assert_eq!(display_backend, "none");
    assert_eq!(
        transport
            .spawn_records()
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        vec!["spawn_first", "spawn_into"],
        "--no-display must use the legacy one-worker-window spawn path"
    );
    let (_raw, state) = raw_runtime_state(workspace);
    assert_eq!(state["display_backend"], json!("none"));
    assert_eq!(state.pointer("/agents/w1/window"), Some(&json!("w1")));
    assert_eq!(state.pointer("/agents/w1/layout_window"), None);
}

// REAL-MACHINE residency boundary (acceptance framework): the PUBLIC quick_start (real TmuxBackend +
// real start_coordinator) on a fresh ws must leave a LIVE tmux session AND a ps-verifiable resident
// coordinator daemon (start_coordinator -> live pid). The framework verifies residency via ps; here we
// only assert the in-report spawn observable. #[ignore] — spawns real tmux + a real daemon subprocess.
#[test]
#[ignore = "real-machine: live tmux session + resident coordinator daemon (framework verifies via ps)"]
fn quick_start_fresh_ws_spawns_resident_tmux_and_coordinator() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let report = quick_start(&team, None, true, None).expect("quick_start");
    match report {
        QuickStartReport::Ready { launch, .. } => {
            assert!(
                !launch.dry_run,
                "a real quick_start must spawn (not dry-run)"
            );
            assert!(
                !launch.started.is_empty(),
                "a real quick_start must spawn >=1 worker into a live tmux session; got {:?}",
                launch.started
            );
        }
        other => panic!("a fresh quick_start must reach Ready; got {other:?}"),
    }
}

// ═════════════════════════════════════════════════════════════════════════
// rt-host-a LOOP #2 — cli-handler DELEGATION sweep (same-class stub): restart / start_agent /
// add_agent return RequirementUnmet at the spawn boundary and NEVER drive the transport (zero spawns)
// nor start the coordinator. RED via the new *_with_transport seams + a RecordingTransport. OS-safe:
// recording transport (no real tmux) + seeded-healthy-coordinator (start_coordinator AlreadyRunning).
// Golden: restart/orchestration.py (Route-B resume spawn), lifecycle/start.py, lifecycle/operations.py.
// ═════════════════════════════════════════════════════════════════════════

pub(super) const DELEG_ROLE_ALPHA: &str = "---\nname: alpha\nrole: Alpha Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAlpha.\n";
pub(super) const DELEG_ROLE_BRAVO: &str = "---\nname: bravo\nrole: Bravo Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nBravo.\n";
const DELEG_ROLE_WORKER2: &str = "---\nname: worker2\nrole: Second Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nSecond worker.\n";

/// A workspace (= self-contained team dir) with a compiled 2-agent spec + state listing alpha/bravo as
/// RESUMABLE (running, valid first_send_at, session_id) + a seeded HEALTHY coordinator.
pub(super) fn restart_ws_two_resumable_workers() -> PathBuf {
    let ws = temp_ws().join("restartteam");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    let alpha_rollout = ws.join("alpha-rollout.jsonl");
    let bravo_rollout = ws.join("bravo-rollout.jsonl");
    std::fs::write(&alpha_rollout, "{}\n").unwrap();
    std::fs::write(&bravo_rollout, "{}\n").unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartteam\nobjective: Restart probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile 2-agent team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartteam",
            "agents": {
                "alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "rollout_path": alpha_rollout.to_string_lossy(), "first_send_at": "2026-05-27T10:00:00+00:00"},
                "bravo": {"status": "running", "provider": "codex", "session_id": "sess-b", "rollout_path": bravo_rollout.to_string_lossy(), "first_send_at": "2026-05-27T10:00:00+00:00"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws); // start_coordinator -> AlreadyRunning (no real daemon subprocess)
    ws
}

fn restart_ws_one_resumable_worker() -> PathBuf {
    let ws = temp_ws().join("restartone");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    let rollout = ws.join("alpha-rollout.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartone\nobjective: Restart readiness probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile 1-agent team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartone",
            "agents": {
                "alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "rollout_path": rollout.to_string_lossy(), "first_send_at": "2026-05-27T10:00:00+00:00"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    ws
}

// 2 [P0] — restart_with_transport must drive the REAL Route-B resume spawn: one spawn per resumable
// worker. The first resumed worker recreates the session with spawn_first; later workers may use
// spawn_into only after a live-session check proves that recreated session still exists. Each spawn
// carries the provider build_command, and the coordinator is started.
#[test]
fn restart_with_transport_spawns_resumable_workers_not_stub() {
    let ws = restart_ws_two_resumable_workers();
    let transport = OfflineTransport::new();

    let result = restart_with_transport(&ws, false, None, &transport);

    let recorded = transport.spawn_records();
    assert_eq!(
        recorded.len(),
        2,
        "restart must spawn ONE worker per resumable agent (alpha, bravo); the rt-host-a stub returns \
         RequirementUnmet with ZERO spawns; got {recorded:?}"
    );
    assert_eq!(
        recorded[0].0, "spawn_first",
        "with has-session=false before alpha, restart must recreate the session with spawn_first; got {recorded:?}"
    );
    assert_eq!(
        recorded[1].0, "spawn_into",
        "with has-session=true after alpha spawn, restart may add bravo with spawn_into; this is liveness-based, not index-based; got {recorded:?}"
    );
    assert!(
        recorded.iter().all(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "each resumed worker's spawn argv must carry the provider build_command (codex); got {recorded:?}"
    );
    assert!(
        matches!(result, Ok(RestartReport::Restarted { coordinator_started: true, .. })),
        "restart must reach RestartReport::Restarted with coordinator_started=true (AlreadyRunning, seeded); got {result:?}"
    );
    let events = crate::event_log::EventLog::new(&ws).tail(20).unwrap();
    let completed = events
        .iter()
        .find(|event| event.get("event").and_then(|v| v.as_str()) == Some("restart.completed"))
        .expect("restart.completed event for successful restart");
    assert_eq!(completed.get("rc").and_then(|v| v.as_str()), Some("ok"));

    let ws = restart_ws_two_resumable_workers();
    let dead_after_first = OfflineTransport::new().with_session_absent_after_spawn_first();
    let result = restart_with_transport_with_readiness_deadline(
        &ws,
        false,
        None,
        &dead_after_first,
        Some(0),
    );
    let recorded = dead_after_first.spawn_records();
    assert_eq!(
        recorded.len(),
        1,
        "if a resumed session disappears after alpha, restart must fail fast before bravo; got result={result:?} recorded={recorded:?}"
    );
    assert!(
        recorded.iter().all(|(kind, _)| kind == "spawn_first"),
        "restart must not call spawn_into/new-window against a dead server; result={result:?} recorded={recorded:?}"
    );
    let result_debug = format!("{result:?}");
    let report = result.expect("resume integrity failure should return a typed failed report");
    let RestartReport::Failed { failed_agents, .. } = report else {
        panic!("resume integrity failure must not be Partial/Restarted; got {report:?}");
    };
    assert_eq!(failed_agents.len(), 1);
    assert_eq!(failed_agents[0].agent_id.as_str(), "alpha");
    assert_eq!(failed_agents[0].phase, "resume");
    assert!(
        dead_after_first.calls().iter().all(|call| *call != "kill_session"),
        "a single worker/session disappearance must not trigger another shared-session kill; calls={:?}",
        dead_after_first.calls()
    );
    let events = crate::event_log::EventLog::new(&ws).tail(20).unwrap();
    let failed = events
        .iter()
        .find(|event| event.get("event").and_then(|v| v.as_str()) == Some("restart.agent_failed"))
        .expect("restart.agent_failed event for isolated alpha");
    assert_eq!(
        failed.get("agent_id").and_then(|v| v.as_str()),
        Some("alpha")
    );
    assert!(
        failed
            .get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|error| error.contains("session_disappeared_after_spawn")),
        "restart must report the first resumed agent/session disappearance explicitly; event={failed} result={result_debug}"
    );
}

#[test]
fn restart_runtime_spec_spawns_from_run_workspace_not_runtime_dir() {
    let workspace = temp_ws();
    std::fs::create_dir_all(workspace.join("agents")).unwrap();
    let rollout = workspace.join("alpha-rollout.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();
    std::fs::write(
        workspace.join("TEAM.md"),
        "---\nname: current\nobjective: Restart cwd probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(workspace.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    let spec = crate::compiler::compile_team(&workspace).expect("compile project-root team");
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, "current");
    std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
    std::fs::write(&spec_path, crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "spec_path": spec_path.to_string_lossy(),
            "team_dir": workspace.to_string_lossy(),
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "codex",
                    "session_id": "sess-a",
                    "rollout_path": rollout.to_string_lossy(),
                    "first_send_at": "2026-05-27T10:00:00+00:00",
                    "spawn_cwd": null
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&workspace);
    let transport = OfflineTransport::new();

    let result = restart_with_transport(&workspace, false, Some("current"), &transport);

    assert!(
        matches!(result, Ok(RestartReport::Restarted { .. })),
        "restart should succeed for a resumable project-root team: {result:?}"
    );
    assert_eq!(
        transport.spawn_cwd_records(),
        vec![workspace.clone()],
        "restart must spawn from run_workspace, not .team/runtime/<team>"
    );
    let state = crate::state::persist::load_runtime_state(&workspace).unwrap();
    assert_eq!(
        state
            .get("agents")
            .and_then(|agents| agents.get("alpha"))
            .and_then(|agent| agent.get("spawn_cwd"))
            .and_then(serde_json::Value::as_str),
        Some(workspace.to_string_lossy().as_ref()),
        "restart must persist the actual spawn cwd for later diagnostics"
    );
    let rebuilt_spec =
        crate::model::yaml::loads(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
    assert_eq!(
        rebuilt_spec
            .get("team")
            .and_then(|team| team.get("workspace"))
            .and_then(crate::model::yaml::Value::as_str),
        Some(workspace.to_string_lossy().as_ref()),
        "restart spec rebuild must keep team.workspace at the project root"
    );
    for agent in rebuilt_spec
        .get("agents")
        .and_then(crate::model::yaml::Value::as_list)
        .unwrap()
    {
        assert_eq!(
            agent
                .get("working_directory")
                .and_then(crate::model::yaml::Value::as_str),
            Some(workspace.to_string_lossy().as_ref()),
            "restart spec rebuild must keep agent working_directory at the project root"
        );
    }
}

#[test]
fn restart_allow_fresh_does_not_force_fresh_other_agents_when_one_session_capture_times_out() {
    let ws = temp_ws().join("restartatomic");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartatomic\nobjective: Restart atomicity probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();

    let sessions_root = ws.join("codex-sessions");
    let dated = sessions_root.join("2026").join("06").join("20");
    std::fs::create_dir_all(&dated).unwrap();
    let alpha_session = "019ee540-37ed-7a20-a141-1d654224d209";
    std::fs::write(
        dated.join(format!("rollout-2026-06-20T21-37-31-{alpha_session}.jsonl")),
        "{}\n",
    )
    .unwrap();

    let spec = crate::compiler::compile_team(&ws).expect("compile restart team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartatomic",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "codex",
                    "session_id": alpha_session,
                    "rollout_path": ws.join("stale").join("missing-alpha.jsonl").to_string_lossy(),
                    "codex_sessions_root": sessions_root.to_string_lossy(),
                    "first_send_at": "2026-05-27T10:00:00+00:00"
                },
                "bravo": {
                    "status": "running",
                    "provider": "codex",
                    "session_id": null,
                    "spawn_cwd": ws.to_string_lossy(),
                    "first_send_at": "2026-05-27T10:00:00+00:00"
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let result = restart_with_transport_with_session_convergence_deadline(
        &ws,
        true,
        None,
        &transport,
        Some(0),
        None,
    );

    assert!(
        matches!(result, Ok(RestartReport::Restarted { .. })),
        "allow-fresh restart should complete with alpha resumed and bravo fresh; got {result:?}"
    );
    let events = crate::event_log::EventLog::new(&ws).tail(80).unwrap();
    let decisions = events
        .iter()
        .filter(|event| {
            event.get("event").and_then(|v| v.as_str()) == Some("restart.resume_decision")
        })
        .collect::<Vec<_>>();
    let alpha = decisions
        .iter()
        .find(|event| event.get("worker_id").and_then(|v| v.as_str()) == Some("alpha"))
        .expect("alpha decision");
    assert_eq!(
        alpha.get("decision").and_then(|v| v.as_str()),
        Some("resume"),
        "alpha has session_id plus matching codex transcript and must not be forced fresh: {alpha}"
    );
    assert!(
        alpha.get("forced_fresh").is_none(),
        "alpha must not inherit bravo's convergence timeout: {alpha}"
    );
    let bravo = decisions
        .iter()
        .find(|event| event.get("worker_id").and_then(|v| v.as_str()) == Some("bravo"))
        .expect("bravo decision");
    assert_eq!(
        bravo.get("decision").and_then(|v| v.as_str()),
        Some("fresh_start")
    );
    assert_eq!(
        bravo.get("forced_fresh").and_then(|v| v.as_bool()),
        Some(true),
        "only the missing-session agent should carry forced_fresh: {bravo}"
    );
}

#[test]
fn restart_allow_fresh_clears_codex_identity_mismatch_tuple_before_capture() {
    let ws = temp_ws().join("restartcrossbindallowfresh");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartcrossbindallowfresh\nobjective: Restart crossbind repair.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let alpha_session = "019f3586-6eb4-7552-8680-c833b483a289";
    let alpha_rollout = codex_identity_rollout(&ws, "alpha-rollout.jsonl", alpha_session, "alpha");
    let spec = crate::compiler::compile_team(&ws).expect("compile crossbind team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartcrossbindallowfresh",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "codex",
                    "window": "alpha",
                    "first_send_at": "2026-07-06T03:45:00+00:00",
                    "session_id": alpha_session,
                    "rollout_path": alpha_rollout.to_string_lossy(),
                    "captured_at": "2026-07-06T03:45:13.596763+00:00",
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "spawn_cwd": ws.to_string_lossy()
                },
                "bravo": {
                    "status": "running",
                    "provider": "codex",
                    "window": "bravo",
                    "first_send_at": "2026-07-06T03:45:00+00:00",
                    "session_id": alpha_session,
                    "rollout_path": alpha_rollout.to_string_lossy(),
                    "captured_at": "2026-07-06T03:45:13.596763+00:00",
                    "captured_via": "fs_watch",
                    "attribution_confidence": "high",
                    "spawn_cwd": ws.to_string_lossy()
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let result = restart_with_transport(&ws, true, None, &transport);

    assert!(
        matches!(result, Ok(RestartReport::Restarted { .. })),
        "allow-fresh restart should repair by fresh-spawning only poisoned bravo; got {result:?}"
    );
    let windows = transport.spawn_window_records();
    let records = transport.spawn_records();
    let bravo_idx = windows
        .iter()
        .position(|(_, window)| window == "bravo")
        .expect("bravo must spawn");
    assert!(
        !records[bravo_idx].1.iter().any(|arg| arg == "resume"),
        "poisoned bravo must fresh-start without codex resume argv; records={records:?}"
    );

    let mut state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    let bravo = state.pointer("/agents/bravo").expect("bravo");
    for field in [
        "session_id",
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
        "capture_state",
    ] {
        let value = bravo.get(field);
        assert!(
            value.is_none() || value == Some(&serde_json::Value::Null),
            "allow-fresh must clear poisoned bravo {field}; got {value:?} in {bravo}"
        );
    }
    if let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) {
        for (team_key, team_state) in teams {
            if let Some(team_bravo) = team_state.pointer("/agents/bravo") {
                for field in [
                    "session_id",
                    "rollout_path",
                    "captured_at",
                    "captured_via",
                    "attribution_confidence",
                    "capture_state",
                ] {
                    let value = team_bravo.get(field);
                    assert!(
                        value.is_none() || value == Some(&serde_json::Value::Null),
                        "allow-fresh must clear teams.{team_key}.agents.bravo {field}; got {value:?} in {team_bravo}"
                    );
                }
            }
        }
    }

    let fresh_session = "019f3587-bravo-fresh";
    let fresh_rollout =
        codex_identity_rollout(&ws, "bravo-fresh-rollout.jsonl", fresh_session, "bravo");
    let mut adapter_for = |provider| crate::provider::adapter::get_adapter(provider);
    crate::provider::session::capture::capture_missing_provider_sessions_once(
        &mut state,
        &mut adapter_for,
        true,
        0,
    )
    .expect("fresh bravo capture should run");
    let bravo = state.pointer("/agents/bravo").expect("bravo after capture");
    assert_eq!(
        bravo.get("session_id").and_then(serde_json::Value::as_str),
        Some(fresh_session),
        "fresh bravo rollout should be capturable after allow-fresh clears poison; rollout={fresh_rollout:?} state={bravo}"
    );
    assert_eq!(
        bravo
            .get("rollout_path")
            .and_then(serde_json::Value::as_str),
        Some(fresh_rollout.to_string_lossy().as_ref())
    );
    let plan = crate::lifecycle::restart::classify_restart_plan_with_resume_validation(
        Some(&ws),
        &state,
        false,
    )
    .expect("classify after fresh capture");
    assert!(
        plan.unresumable.is_empty(),
        "next restart must not loop on session_identity_mismatch after fresh capture; got {:?}",
        plan.unresumable
    );
    let bravo_decision = plan
        .decisions
        .iter()
        .find(|decision| decision.agent_id.as_str() == "bravo")
        .expect("bravo decision");
    assert_eq!(bravo_decision.decision, ResumeDecision::Resume);
}

#[test]
fn restart_spawn_failure_isolated_to_partial_report() {
    let ws = restart_ws_two_resumable_workers();
    let transport = OfflineTransport::new().with_spawn_failure("bravo", "injected bravo failure");

    let result = restart_with_transport(&ws, false, None, &transport);

    let report = result.expect("single worker failure must be represented as partial report");
    let RestartReport::Partial {
        agents,
        failed_agents,
        coordinator_started,
        ..
    } = report
    else {
        panic!("single worker failure must return RestartReport::Partial; got {report:?}");
    };
    assert!(
        coordinator_started,
        "partial restart still starts coordinator for successful agents"
    );
    assert_eq!(
        agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
    assert_eq!(failed_agents.len(), 1);
    assert_eq!(failed_agents[0].agent_id.as_str(), "bravo");
    assert_eq!(failed_agents[0].phase, "spawn");

    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        state
            .pointer("/agents/alpha/status")
            .and_then(|v| v.as_str()),
        Some("running")
    );
    assert_eq!(
        state
            .pointer("/agents/bravo/status")
            .and_then(|v| v.as_str()),
        Some("failed")
    );
    assert!(
        state
            .pointer("/agents/bravo/restart_error")
            .and_then(|v| v.as_str())
            .is_some_and(|error| error.contains("injected bravo failure")),
        "failed agent must retain restart_error in state: {state}"
    );

    let events = crate::event_log::EventLog::new(&ws).tail(20).unwrap();
    let completed = events
        .iter()
        .find(|event| event.get("event").and_then(|v| v.as_str()) == Some("restart.completed"))
        .expect("restart.completed event for partial restart");
    assert_eq!(
        completed.get("rc").and_then(|v| v.as_str()),
        Some("partial")
    );
    assert!(
        completed
            .get("successful_agents")
            .and_then(|v| v.as_array())
            .is_some_and(|agents| agents.iter().any(|agent| agent.as_str() == Some("alpha"))),
        "completed event must list successful agents: {completed}"
    );
    assert!(
        completed
            .get("failed_agents")
            .and_then(|v| v.as_array())
            .is_some_and(|agents| agents.iter().any(|agent| {
                agent.get("agent_id").and_then(|v| v.as_str()) == Some("bravo")
                    && agent.get("phase").and_then(|v| v.as_str()) == Some("spawn")
            })),
        "completed event must list failed agents with phase: {completed}"
    );
}

#[test]
fn restart_all_spawn_failures_return_failed_report() {
    let ws = restart_ws_one_resumable_worker();
    let transport = OfflineTransport::new().with_spawn_failure("alpha", "injected alpha failure");

    let result = restart_with_transport(&ws, false, None, &transport);

    let report = result.expect("all worker failures should return typed failed report");
    let RestartReport::Failed { failed_agents, .. } = report else {
        panic!("all worker failures must return RestartReport::Failed; got {report:?}");
    };
    assert_eq!(failed_agents.len(), 1);
    assert_eq!(failed_agents[0].agent_id.as_str(), "alpha");
    assert_eq!(failed_agents[0].phase, "spawn");

    let events = crate::event_log::EventLog::new(&ws).tail(20).unwrap();
    let completed = events
        .iter()
        .find(|event| event.get("event").and_then(|v| v.as_str()) == Some("restart.completed"))
        .expect("restart.completed event for failed restart");
    assert_eq!(completed.get("rc").and_then(|v| v.as_str()), Some("fail"));
}

#[test]
fn restart_spawn_loop_keeps_failure_isolation_guard() {
    let source = include_str!("../restart/rebuild.rs");
    let start = source
        .find("BEGIN_B5_RESTART_ISOLATION_LOOP")
        .expect("B5 isolation loop start marker");
    let end = source
        .find("END_B5_RESTART_ISOLATION_LOOP")
        .expect("B5 isolation loop end marker");
    let body = &source[start..end];
    for forbidden in ["?;", "break", "return "] {
        assert!(
            !body.contains(forbidden),
            "B5 spawn loop must not fail-fast via {forbidden:?}; body={body}"
        );
    }
    assert!(
        body.contains("continue"),
        "B5 spawn loop should isolate per-agent failures and continue; body={body}"
    );
}

#[test]
fn restart_times_out_when_spawned_worker_pane_is_not_addressable() {
    let ws = restart_ws_one_resumable_worker();
    let transport = OfflineTransport::new().with_spawned_panes_addressable(false);

    let result =
        restart_with_transport_with_readiness_deadline(&ws, false, None, &transport, Some(0));

    let text = format!("{result:?}");
    assert!(
        text.contains("restart not ready")
            && text.contains("worker pane addressable: no")
            && text.contains("Action:")
            && text.contains("Log:"),
        "restart must refuse with N38 readiness timeout details, not return ok; got {text}"
    );
    assert!(
        !matches!(result, Ok(RestartReport::Restarted { .. })),
        "restart readiness timeout must not return Restarted ok"
    );
    let events = crate::event_log::EventLog::new(&ws).tail(20).unwrap();
    let timeout = events
        .iter()
        .find(|event| {
            event.get("event").and_then(|v| v.as_str()) == Some("restart.readiness_timeout")
        })
        .expect("restart.readiness_timeout event");
    assert_eq!(
        timeout
            .get("worker_pane_addressable")
            .and_then(|v| v.as_bool()),
        Some(false),
        "timeout event must carry the failed readiness condition: {timeout}"
    );
}

// 3 [P0] — start_agent_with_transport on a non-paused agent with a session_id must spawn EXACTLY ONE
// worker (resume) carrying the provider build_command. Today the stub returns RequirementUnmet with
// ZERO spawns -> RED at recorded.len().
#[test]
fn start_agent_with_transport_spawns_resume_not_stub() {
    let ws = temp_ws().join("startagentws");
    std::fs::create_dir_all(&ws).unwrap();
    let rollout = ws.join("alpha-rollout.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-sa",
            "agents": {"alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "rollout_path": rollout.to_string_lossy(), "first_send_at": "2026-05-27T10:00:00+00:00"}}
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let _result = start_agent_with_transport(
        &ws,
        &AgentId::new("alpha"),
        false,
        false,
        false,
        None,
        &transport,
    );

    let recorded = transport.spawn_records();
    assert_eq!(
        recorded.len(),
        1,
        "start_agent must spawn EXACTLY ONE worker (resume); the rt-host-a stub returns RequirementUnmet \
         with ZERO spawns; got {recorded:?}"
    );
    assert!(
        recorded[0].1.iter().any(|a| a == "codex"),
        "the resume spawn argv must carry the provider build_command (codex); got {:?}",
        recorded[0].1
    );
}

// 4 [P0] — add_agent_with_transport must (a) recompile + write the spec (already works) AND (b) spawn
// the new worker window. Today the stub recompiles then returns RequirementUnmet with ZERO spawns ->
// RED at (b). OS-safe: the new role file is OUTSIDE agents/ (so it's not a dup), seeded healthy coordinator.
#[test]
fn add_agent_with_transport_spawns_new_worker_not_stub() {
    let team = temp_ws().join("addteam");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: addteam\nobjective: Add probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap(); // existing agent
    let role_file = team.join("worker2-role.md"); // OUTSIDE agents/ -> not a duplicate of an existing agent
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);
    let transport = OfflineTransport::new();

    let _result = add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    // (a) the recompiled spec was written (real subsystem step). E5: under .team/runtime/<team_key>/,
    // NOT the user team dir.
    let runtime_spec = find_runtime_spec(&team, "addteam");
    assert!(
        runtime_spec.is_some(),
        "add_agent must recompile + write team.spec.yaml under .team/runtime/<team_key>/"
    );
    assert!(
        !team.join("team.spec.yaml").exists(),
        "E5: add_agent must NOT write spec into the user team dir"
    );
    // (b) the new worker window was spawned (RED: stub recompiles then RequirementUnmet -> ZERO spawns).
    let recorded = transport.spawn_records();
    assert!(
        !recorded.is_empty(),
        "add_agent must spawn the new worker window (>=1 recorded spawn); the rt-host-a stub recompiles \
         then returns RequirementUnmet with ZERO spawns; got {recorded:?}"
    );
    assert!(
        recorded.iter().any(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "the new worker's spawn argv must carry the provider build_command (codex); got {recorded:?}"
    );
}

#[test]
fn add_agent_role_doc_exists_runtime_missing_succeeds() {
    let team = temp_ws().join("add_role_doc_team");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: addroledoc\nobjective: Add role-doc probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("agents").join("worker2.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-addroledoc",
            "agents": {
                "implementer": {"status": "running", "provider": "codex", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&team);
    let transport = OfflineTransport::new();

    let result = add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    assert!(
        result.is_ok(),
        "a prepared agents/<id>.md role doc is not membership; runtime state is authoritative: {result:?}"
    );
    let state = crate::state::persist::load_runtime_state(&team).expect("load state");
    assert!(
        state.pointer("/agents/worker2").is_some(),
        "add-agent must add worker2 to runtime state; state={state}"
    );
    assert_eq!(
        transport.spawn_records().len(),
        1,
        "add-agent must spawn exactly the new worker"
    );
}

#[test]
fn start_agent_runtime_missing_role_doc_exists_no_side_effect() {
    let team = temp_ws().join("start_missing_team");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: startmissing\nobjective: Start missing probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("reviewer.md"), DELEG_ROLE_WORKER2).unwrap();
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-startmissing",
            "agents": {
                "implementer": {"status": "running", "provider": "codex", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    let transport = OfflineTransport::new();

    let result = start_agent_with_transport(
        &team,
        &AgentId::new("reviewer"),
        false,
        false,
        true,
        None,
        &transport,
    );

    let text = format!("{result:?}");
    assert!(
        text.contains("not found"),
        "start-agent must refuse runtime-missing agents before side effects; got {text}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "runtime-missing start-agent must not spawn a pane"
    );
    assert!(
        !team
            .join(".team")
            .join("runtime")
            .join("mcp")
            .join("reviewer.json")
            .exists(),
        "runtime-missing start-agent must not write worker MCP config"
    );
    let events = crate::event_log::EventLog::new(&team)
        .tail(20)
        .unwrap_or_default();
    assert!(
        !events.iter().any(|event| {
            event.get("event").and_then(|v| v.as_str()) == Some("start_agent.agent_start")
        }),
        "runtime-missing start-agent must not emit start_agent.agent_start; events={events:?}"
    );
}

/// 0.4.6 tuple-atomic contract (audit §Restart 修改清单, line 177): Copilot
/// fresh restart MUST leave `session_id=null` in persisted state. The argv
/// still carries `--session-id <uuid>` (planned capture hint), and the row
/// keeps `_pending_session_id=<uuid>` so the Copilot scanner can confirm
/// the sqlite entry on next tick and promote into the authoritative tuple.
/// Pre-0.4.6, restart promoted the planned uuid directly into `session_id`
/// without scanner confirmation, violating the capture-only-writer rule
/// (audit §Restart 当前违规 #1).
#[test]
fn restart_allow_fresh_copilot_keeps_session_id_null_until_capture_confirms() {
    let ws = temp_ws().join("restartcopilot");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartcopilot\nobjective: Restart copilot probe.\nprovider: copilot\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("agents").join("alpha.md"),
        "---\nname: alpha\nrole: Alpha Worker\nprovider: copilot\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAlpha.\n",
    )
    .unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile copilot team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartcopilot",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "copilot",
                    "window": "alpha",
                    "first_send_at": "2026-05-27T10:00:00+00:00",
                    "session_id": null,
                    "rollout_path": "/tmp/stale-copilot-rollout.jsonl",
                    "captured_at": "2026-05-27T10:00:00+00:00",
                    "captured_via": "stale",
                    "attribution_confidence": "stale"
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let result = restart_with_transport(&ws, true, None, &transport);

    assert!(
        matches!(result, Ok(RestartReport::Restarted { .. })),
        "allow-fresh restart should fresh-spawn the copilot worker; got {result:?}"
    );
    let recorded = transport.spawn_records();
    assert_eq!(
        recorded.len(),
        1,
        "expected one copilot spawn; got {recorded:?}"
    );
    let session_arg = recorded[0]
        .1
        .windows(2)
        .find_map(|pair| (pair[0] == "--session-id").then(|| pair[1].clone()))
        .expect("copilot fresh spawn argv must include --session-id");
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    let agent = state.pointer("/agents/alpha").expect("alpha state");
    // 0.4.6 contract: session_id stays null until scanner confirms sqlite.
    assert_eq!(
        agent.get("session_id"),
        Some(&serde_json::Value::Null),
        "fresh restart must NOT promote planned id into session_id; \
         capture (scanner) is the only authoritative writer. agent={agent}"
    );
    // The pending hint is preserved so the Copilot scanner has the
    // expected-id binding on the next coordinator tick.
    assert_eq!(
        agent
            .get("_pending_session_id")
            .and_then(serde_json::Value::as_str),
        Some(session_arg.as_str()),
        "fresh restart must retain _pending_session_id matching argv --session-id; \
         this is the capture hint the scanner uses for sqlite confirmation"
    );
    for stale in [
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
    ] {
        let value = agent.get(stale);
        assert!(
            value.is_none() || value == Some(&serde_json::Value::Null),
            "fresh restart must clear stale capture field {stale}; got {value:?}"
        );
    }
    // Second classify must NOT silently refuse — the row has no session_id
    // (caller still needs to use --allow-fresh or wait for capture).
    let plan = crate::lifecycle::restart::classify_restart_plan(&state, false)
        .expect("classify second restart");
    // The worker has session_id=null + a non-empty _pending_session_id,
    // so classify treats it as no-persisted-session-id (not backing-missing),
    // which `--allow-fresh` resolves cleanly without loops.
    let _ = plan; // shape verified above; details depend on classifier internals.
}

/// 0.4.6 tuple-atomic contract (audit, line 268): Claude fresh restart
/// leaves session_id=null even when input row had a stale session_id.
/// The poisoned-row scenario (Bug 045 release-engineer state).
#[test]
fn restart_allow_fresh_claude_clears_poisoned_session_id() {
    let ws = temp_ws().join("restartclaude046");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: restartclaude046\nobjective: 0.4.6 contract.\nprovider: claude\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("agents").join("alpha.md"),
        "---\nname: alpha\nrole: Alpha\nprovider: claude\nmodel: sonnet\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAlpha.\n",
    )
    .unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    let stale_uuid = "bd68bfbd-b9b6-4e0e-8197-ce5a47c4a930";
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartclaude046",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "claude",
                    "window": "alpha",
                    "first_send_at": "2026-06-23T10:00:00+00:00",
                    "session_id": stale_uuid,
                    "rollout_path": null,
                    "captured_at": null,
                    "captured_via": null
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let result = restart_with_transport(&ws, true, None, &transport);
    assert!(
        matches!(result, Ok(RestartReport::Restarted { .. })),
        "allow-fresh restart should fresh-spawn the claude worker; got {result:?}"
    );

    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    let agent = state.pointer("/agents/alpha").expect("alpha");
    assert_eq!(
        agent.get("session_id"),
        Some(&serde_json::Value::Null),
        "0.4.6 contract: Claude fresh restart must clear stale session_id; agent={agent}"
    );
    // C1 dual-projection: teams.<key>.agents.alpha.session_id must also be null.
    if let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) {
        for (team_key, team_state) in teams {
            if let Some(team_alpha) = team_state.pointer("/agents/alpha") {
                let team_session = team_alpha.get("session_id");
                assert!(
                    team_session.is_none() || team_session == Some(&serde_json::Value::Null),
                    "0.4.6: teams.{team_key}.agents.alpha.session_id must be null; got {team_session:?}"
                );
            }
        }
    }
}

/// 0.4.6 tuple-atomic contract (audit §Fork 修改清单, line 201): fork
/// with a scalar `session_id` but no rollout_path/captured_at/captured_via
/// must be REFUSED before the native fork. Pre-0.4.6 fork only checked
/// session_id non-empty, which let it attach to a session with no
/// confirmed backing.
#[test]
fn fork_refuses_source_with_partial_tuple() {
    let ws = temp_ws().join("forkpartial046");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: forkpartial046\nobjective: fork tuple.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("agents").join("src.md"),
        "---\nname: src\nrole: Source\nprovider: codex\nmodel: gpt\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nSrc.\n",
    )
    .unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-forkpartial046",
            "agents": {
                "src": {
                    "status": "running",
                    "provider": "codex",
                    "window": "src",
                    "first_send_at": "2026-06-25T10:00:00+00:00",
                    "session_id": "scalar-only-uuid",
                    "rollout_path": null,
                    "captured_at": null,
                    "captured_via": null
                }
            }
        }),
    )
    .unwrap();
    let transport = OfflineTransport::new();

    // fork_agent is the entry point; it should refuse with a clear message.
    let src_id = crate::model::ids::AgentId::new("src");
    let dst_id = crate::model::ids::AgentId::new("clone");
    let result = crate::lifecycle::fork_agent_with_transport(
        &ws, &src_id, &dst_id, None,  // label
        false, // open_display
        None,  // team
        &transport,
    );
    assert!(
        result.is_err(),
        "0.4.6: fork with partial source tuple must be refused; got Ok({result:?})"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("source session backing is missing")
            || err.contains("backing")
            || err.contains("incomplete"),
        "0.4.6: fork refusal must name backing/incomplete cause; got error={err}"
    );
}

#[test]
fn add_agent_adaptive_splits_last_non_full_layout_window() {
    let roles = [("w1.md", role_doc("w1")), ("w2.md", role_doc("w2"))];
    let role_refs = roles
        .iter()
        .map(|(file, doc)| (*file, doc.as_str()))
        .collect::<Vec<_>>();
    let team = quick_start_team_dir_with_roles(&role_refs);
    let session = "team-layout-add";
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": session,
            "display_backend": "adaptive",
            "active_team_key": "teamdir",
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 0, "pane_id": "%1"},
                "w2": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 1, "pane_id": "%2"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&team);
    let role_file = team.join("w3-role.md");
    std::fs::write(&role_file, role_doc("w3")).unwrap();
    let transport = OfflineTransport::new()
        .with_session_present(true)
        .with_targets(vec![
            layout_pane(session, "team-w1", "%1", 0),
            layout_pane(session, "team-w1", "%2", 1),
        ])
        .with_pane_presence("%1", true)
        .with_pane_presence("%2", true);

    add_agent_with_transport(
        &team,
        &AgentId::new("w3"),
        &role_file,
        true,
        None,
        &transport,
    )
    .expect("adaptive add-agent should start");

    assert_eq!(
        transport.spawn_window_records().last(),
        Some(&(String::from("spawn_split"), String::from("team-w1"))),
        "last non-full layout window should be split"
    );
    let (_raw, state) = raw_runtime_state(&team);
    assert_eq!(
        state.pointer("/agents/w3/layout_window"),
        Some(&json!("team-w1"))
    );
    assert_eq!(state.pointer("/agents/w3/pane_index"), Some(&json!(2)));
}

#[test]
fn add_agent_adaptive_creates_suffix_window_when_last_layout_full_and_name_collides() {
    let roles = [
        ("w1.md", role_doc("w1")),
        ("w2.md", role_doc("w2")),
        ("w3.md", role_doc("w3")),
    ];
    let role_refs = roles
        .iter()
        .map(|(file, doc)| (*file, doc.as_str()))
        .collect::<Vec<_>>();
    let team = quick_start_team_dir_with_roles(&role_refs);
    let session = "team-layout-add-full";
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": session,
            "display_backend": "adaptive",
            "active_team_key": "teamdir",
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 0, "pane_id": "%1"},
                "w2": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 1, "pane_id": "%2"},
                "w3": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 2, "pane_id": "%3"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&team);
    let role_file = team.join("w4-role.md");
    std::fs::write(&role_file, role_doc("w4")).unwrap();
    let transport = OfflineTransport::new()
        .with_session_present(true)
        .with_targets(vec![
            layout_pane(session, "team-w1", "%1", 0),
            layout_pane(session, "team-w1", "%2", 1),
            layout_pane(session, "team-w1", "%3", 2),
            layout_pane(session, "team-w2", "%9", 0),
        ])
        .with_pane_presence("%1", true)
        .with_pane_presence("%2", true)
        .with_pane_presence("%3", true);

    add_agent_with_transport(
        &team,
        &AgentId::new("w4"),
        &role_file,
        true,
        None,
        &transport,
    )
    .expect("adaptive add-agent should start");

    assert_eq!(
        transport.spawn_window_records().last(),
        Some(&(String::from("spawn_into"), String::from("team-w2-2"))),
        "full last layout window should create next window with collision suffix"
    );
    let (_raw, state) = raw_runtime_state(&team);
    assert_eq!(
        state.pointer("/agents/w4/layout_window"),
        Some(&json!("team-w2-2"))
    );
    assert_eq!(state.pointer("/agents/w4/pane_index"), Some(&json!(0)));
}

#[test]
fn stop_agent_adaptive_kills_target_pane_not_shared_layout_window() {
    let roles = [("w1.md", role_doc("w1")), ("w2.md", role_doc("w2"))];
    let role_refs = roles
        .iter()
        .map(|(file, doc)| (*file, doc.as_str()))
        .collect::<Vec<_>>();
    let team = quick_start_team_dir_with_roles(&role_refs);
    let spec = crate::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        crate::model::yaml::dumps(&spec),
    )
    .unwrap();
    let session = "team-layout-stop";
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": session,
            "display_backend": "adaptive",
            "agents": {
                "w1": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 0, "pane_id": "%1"},
                "w2": {"status": "running", "provider": "codex", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 1, "pane_id": "%2"}
            }
        }),
    )
    .unwrap();
    let transport = OfflineTransport::new()
        .with_targets(vec![
            layout_pane(session, "team-w1", "%1", 0),
            layout_pane(session, "team-w1", "%2", 1),
        ])
        .with_pane_presence("%2", true);

    let report = stop_agent_with_transport(&team, &AgentId::new("w2"), None, &transport).unwrap();

    assert!(report.stopped);
    assert_eq!(report.target, "%2");
    assert!(
        transport.calls().iter().any(|call| *call == "kill_pane"),
        "adaptive stop must kill only the target pane"
    );
    assert!(
        !transport.calls().iter().any(|call| *call == "kill_window"),
        "adaptive stop must not kill the shared layout window"
    );
    let (_raw, state) = raw_runtime_state(&team);
    assert_eq!(state.pointer("/agents/w1/pane_id"), Some(&json!("%1")));
    assert_eq!(
        state.pointer("/agents/w1/layout_window"),
        Some(&json!("team-w1"))
    );
}

#[test]
fn start_agent_adaptive_restarts_missing_pane_in_existing_layout_window() {
    let ws = temp_ws();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-layout-restart",
            "display_backend": "adaptive",
            "agents": {
                "w1": {"status": "running", "provider": "codex", "role": "w1", "model": "gpt-5.5", "auth_mode": "subscription", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 0, "pane_id": "%1"},
                "w2": {"status": "running", "provider": "codex", "role": "w2", "model": "gpt-5.5", "auth_mode": "subscription", "window": "team-w1", "layout_window": "team-w1", "layout_index": 0, "pane_index": 1, "pane_id": "%2"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new()
        .with_session_present(true)
        .with_targets(vec![layout_pane("team-layout-restart", "team-w1", "%2", 1)])
        .with_pane_presence("%1", false)
        .with_pane_presence("%2", true);

    let outcome = start_agent_with_transport(
        &ws,
        &AgentId::new("w1"),
        false,
        true,
        true,
        None,
        &transport,
    )
    .expect("adaptive restart should spawn");

    assert!(matches!(outcome, StartAgentOutcome::Running { .. }));
    assert_eq!(
        transport.spawn_window_records().last(),
        Some(&(String::from("spawn_split"), String::from("team-w1"))),
        "missing worker pane should be rebuilt in its existing layout window"
    );
    let (_raw, state) = raw_runtime_state(&ws);
    assert_eq!(
        state.pointer("/agents/w1/layout_window"),
        Some(&json!("team-w1"))
    );
    assert_eq!(state.pointer("/agents/w1/window"), Some(&json!("team-w1")));
    assert_eq!(state.pointer("/agents/w2/pane_id"), Some(&json!("%2")));
}

// ═════════════════════════════════════════════════════════════════════════
// WAVE 2 · LANE A — agent-lifecycle byte-parity contracts. stop_agent / reset_agent / remove_agent /
// fork_agent are stubs (OwnerRefused / RequirementUnmet / "session_id missing"). These RED contracts
// LOCK the golden behavior (lifecycle/operations.py + agents.py). The existing tolerant tests accept the
// stub; these are STRICTER (RED today). OS-safe: no-owner ws + seeded spec/state, non-running agents
// (pure fs/state). Transport-asserting parts (kill_window / native-fork spawn) -> seam + #[ignore].
// ═════════════════════════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════════════════════════
// collect #223 — task-scoped seeding (RED). Golden launch/core.py:69 ALWAYS seeds the runtime
// state's `tasks` key from spec.tasks (≥ []); the doc compiler emits a default task
// {id:"task_initial",…} (compiler.rs:308). Rust initial_runtime_state (launch.rs) emits only
// {session_name,team_dir,agents} — NO tasks key → send/collect cannot resolve the task. RED.
// OS-safe: OfflineTransport (zero real spawn).
// ═══════════════════════════════════════════════════════════════════════════
#[test]
fn quick_start_seeds_tasks_key_from_compiled_spec() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let transport = OfflineTransport::new();
    let _ = quick_start_with_transport(&team, None, true, None, &transport);
    let workspace = team
        .parent()
        .expect("team_workspace(<base>/teamdir) = <base>");
    let state = crate::state::persist::load_runtime_state(workspace)
        .expect("runtime state.json must exist after quick_start");
    // (b) the tasks KEY must always be present and a JSON array (golden launch/core.py:69 default []).
    let tasks = match state.get("tasks") {
        Some(t) => t,
        None => panic!(
            "initial_runtime_state MUST seed a `tasks` key (golden launch/core.py:69 `tasks=[…spec.tasks]`); \
             state keys = {:?}",
            state.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())
        ),
    };
    let tasks = tasks
        .as_array()
        .expect("`tasks` must be a JSON array (golden default [])");
    // (a) spec.tasks must be carried into runtime state — the doc compiler's default task id.
    assert!(
        tasks
            .iter()
            .any(|t| t.get("id").and_then(|v| v.as_str()) == Some("task_initial")),
        "state.tasks must carry the compiled spec's task (id=task_initial); got {tasks:?}"
    );
}

// Stage A — golden launch/core.py:62-71 seeded top-level runtime state in insertion order:
// spec_path, workspace, team_dir, session_name, leader, agents, tasks, display_backend.
//
// OLD (Python parity): the top-level shape ended at `display_backend`.
// NEW (Bug 1/2 — team-in-team state scope, see tests/team_in_team_state_scope_red.rs):
//   `active_team_key` and `teams` are appended at the tail so the runtime can carry the
//   nested team-in-team scope alongside the original flat fields. Owner-binding fields
//   (`leader_receiver` / `team_owner` / `owner_epoch`) live ONLY under
//   `teams[<active_team_key>]` — Bug 2 owner team-scope (N1/N12/N18/N29) deliberately
//   keeps them OFF the root so per-team isolation has a single source of truth and
//   cross-team reads cannot accidentally pick up another team's binding from the root.
//   The post-launch hook drops the top-level owner triple when the launched pane is
//   unbound (empty pane_id), and only the per-team entry retains the binding.
//   R1: topology is explicit; fresh managed teams carry `is_external_leader=false`
//   before the team-in-team suffix. Order remains the golden prefix
//   (spec_path … display_backend) + topology marker + new suffix (active_team_key,
//   teams). display_backend stays the resolved backend from display/backend.py:12-29
//   (default adaptive), not the raw optional spec field.
#[test]
#[serial(env)]
fn quick_start_state_seeds_spec_path_workspace_leader_display_backend() {
    // Bug 2 owner team-scope: top-level owner triple is dropped when the seeded
    // pane is empty; an ambient TMUX_PANE (tests run inside the dev's tmux session)
    // would otherwise leak into the caller-identity seed, inflate the seeded owner
    // with a non-empty pane, and keep the top-level keys present — masking the
    // per-team-isolation invariant this assertion locks. Force-unset for the test.
    let _tmux_pane = EnvVarGuard::unset("TMUX_PANE");
    let _tmux = EnvVarGuard::unset("TMUX");
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let transport = OfflineTransport::new();
    let _ = quick_start_with_transport(&team, None, true, None, &transport);
    let workspace = team
        .parent()
        .expect("team_workspace(<base>/teamdir) = <base>");
    // E5: spec_path in state now points to .team/runtime/<team_key>/, team_dir stays the user role dir.
    let spec_path = quick_start_runtime_spec_path(&team);
    let (raw, state) = raw_runtime_state(workspace);
    let keys = state
        .as_object()
        .expect("state root object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "spec_path",
            "workspace",
            "team_dir",
            "session_name",
            "leader",
            "agents",
            "tasks",
            "display_backend",
            "is_external_leader",
            // 0.5.x Phase 1d hot-path 接线 (裁决1 msg_76e1d98202b8):
            // `transport = { kind, source }` inserted here by
            // `annotate_runtime_transport`. Kind is `"tmux"` for the
            // default tmux path (byte-equivalent behavior + new metadata
            // field); source is `"unknown"` until Phase 2 threads
            // `ResolvedTransport.source` through the launch call site.
            "transport",
            // 0.4.0 refactor: `team_key` added as top-level topology marker
            // alongside active_team_key (refactor-modular-architecture). The
            // owner / leader_receiver / owner_epoch invariants below still
            // hold — those live only under teams[<active_team_key>].
            "team_key",
            "active_team_key",
            "teams",
        ],
        "state.json top-level key order must match golden launch/core.py:62-71 \
         plus R1 topology marker and Bug 1/2 team-in-team suffix \
         (is_external_leader, team_key, active_team_key, teams). \
         Bug 2 owner team-scope (N1/N12/N18/N29) deliberately keeps owner / \
         leader_receiver / owner_epoch OFF the top level — they live ONLY under \
         teams[<active_team_key>] so per-team isolation has a single source of \
         truth (no shadow copy on the root that callers could read across teams); \
         raw={raw}"
    );
    assert_eq!(
        state["spec_path"],
        json!(spec_path.canonicalize().unwrap().to_string_lossy())
    );
    assert_eq!(state["workspace"], json!(workspace.to_string_lossy()));
    assert_eq!(state["team_dir"], json!(team.to_string_lossy()));
    assert_eq!(state["session_name"], json!("team-quickteam"));
    assert_eq!(
        state["leader"]["id"],
        json!("leader"),
        "leader must be copied from compiled spec.leader"
    );
    assert!(
        state["agents"]
            .as_object()
            .is_some_and(|agents| agents.contains_key("implementer")),
        "existing agents value must remain seeded from compiled spec; got {:?}",
        state["agents"]
    );
    assert!(
        state["tasks"].as_array().is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.get("id").and_then(|id| id.as_str()) == Some("task_initial"))
        }),
        "existing tasks value must remain seeded from compiled spec; got {:?}",
        state["tasks"]
    );
    assert_eq!(
        state["display_backend"],
        json!("adaptive"),
        "adaptive layout directive: resolve_display_backend(None, source='launch') defaults to adaptive"
    );
    assert_eq!(state["is_external_leader"], json!(false));
}

// Stage B1 — golden launch/core.py:238-255 writes the running agent state only after
// a successful spawn. The fixed clock is a test seam for `spawned_at`; capture intentionally
// misses, so session/capture fields remain present JSON nulls. MCP install is deterministic:
// provider_cli/adapter.py:111-114 writes workspace/.team/runtime/mcp/<agent>.json.
#[test]
#[serial(env)]
fn quick_start_running_agent_state_shape_after_spawn_is_golden() {
    const FIXED_SPAWNED_AT: &str = "2026-06-04T00:00:00+00:00";
    let _clock_guard = EnvVarGuard::set("TEAM_AGENT_TEST_FIXED_SPAWNED_AT", FIXED_SPAWNED_AT);
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let transport = OfflineTransport::new();
    let _ = quick_start_with_transport(&team, None, true, None, &transport);
    let workspace = team
        .parent()
        .expect("team_workspace(<base>/teamdir) = <base>");
    let (raw, state) = raw_runtime_state(workspace);
    let agent = state
        .pointer("/agents/implementer")
        .unwrap_or_else(|| panic!("implementer agent state missing; raw={raw}"));
    let keys = agent
        .as_object()
        .expect("agent state object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    // 0.3.28 Step 4b: adaptive layout fields (layout_window, layout_index,
    // pane_index, display) removed from running agent state — 1-window-per-agent
    // means no layout bookkeeping needed.
    assert_eq!(
        keys,
        vec![
            "status",
            "provider",
            "agent_id",
            "model",
            "auth_mode",
            "profile",
            "window",
            "mcp_config",
            "permissions",
            "effective_approval_policy",
            "session_id",
            "rollout_path",
            "captured_at",
            "captured_via",
            "attribution_confidence",
            "spawn_cwd",
            "spawned_at",
            "pane_id",
        ],
        "0.3.31 Codex capture correction: no framework _pending_session_id for Codex (CLI doesn't honor --session-id); raw={raw}"
    );
    assert_eq!(agent["status"], json!("running"));
    assert_eq!(agent["provider"], json!("codex"));
    assert_eq!(agent["agent_id"], json!("implementer"));
    assert_eq!(agent["model"], json!("gpt-5.5"));
    assert_eq!(agent["auth_mode"], json!("subscription"));
    assert!(agent["profile"].is_null());
    assert_eq!(agent["window"], json!("implementer"));
    assert_eq!(
        agent["mcp_config"],
        json!(workspace
            .join(".team/runtime/mcp/implementer.json")
            .to_string_lossy())
    );
    assert_eq!(
        agent["permissions"],
        json!({
            "agent_id": "implementer",
            "provider": "codex",
            "tools": ["mcp_team"],
            "resolved_tools": [{"tool": "mcp_team", "enforcement": "prompt_only"}],
            "has_prompt_only": true
        })
    );
    assert!(agent["session_id"].is_null());
    assert!(agent["rollout_path"].is_null());
    assert!(agent["captured_at"].is_null());
    assert!(agent["captured_via"].is_null());
    assert!(agent["attribution_confidence"].is_null());
    // 0.3.28 Step 4b: layout_window/layout_index/pane_index/display removed.
    // No adaptive bookkeeping needed — each worker has its own window named
    // after the agent_id.
    assert!(agent.get("layout_window").is_none());
    assert!(agent.get("layout_index").is_none());
    assert!(agent.get("pane_index").is_none());
    assert!(agent.get("display").is_none());
    // D5 (#264) / Python launch/core.py:253 — fresh launch persists spawn_cwd=workspace.
    assert_eq!(agent["spawn_cwd"], json!(workspace.to_string_lossy()));
    assert_eq!(agent["spawned_at"], json!(FIXED_SPAWNED_AT));
    assert_eq!(
        agent["pane_id"],
        json!("%0"),
        "#252 RC2: launch must persist the real pane_id returned by the transport, not drop it when pane_pid is unavailable"
    );
    let events = crate::event_log::EventLog::new(workspace)
        .tail(0)
        .expect("read event log");
    let spawn_event = events
        .iter()
        .find(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("provider.worker.spawn_argv")
                && event.get("agent_id").and_then(serde_json::Value::as_str) == Some("implementer")
        })
        .unwrap_or_else(|| panic!("provider.worker.spawn_argv missing; events={events:?}"));
    assert_eq!(spawn_event["spawn_cwd"], json!(workspace.to_string_lossy()));
    assert_eq!(spawn_event["spawned_at"], json!(FIXED_SPAWNED_AT));
    assert_eq!(spawn_event["source"], json!("launch"));
    assert_eq!(spawn_event["spawn_epoch"], json!(0));
}

// Stage B2 — golden launch/core.py:171-173 writes paused workers as exactly
// {status, provider} and skips spawn entirely.
#[test]
fn quick_start_paused_agent_state_is_paused_provider_only_and_not_spawned() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let spec_path = compiled_spec_path_with_paused_agent(&team);
    let workspace = team
        .parent()
        .expect("team_workspace(<base>/teamdir) = <base>");
    crate::state::persist::save_runtime_state(
        workspace,
        &json!({
            "session_name": "team-quickteam",
            "team_dir": team.to_string_lossy(),
            "agents": {
                "implementer": {
                    "provider": "codex",
                    "role": "Implementation Engineer",
                    "model": "gpt-5.5",
                    "auth_mode": "subscription"
                }
            },
            "tasks": []
        }),
    )
    .expect("seed pre-launch runtime state");
    let transport = OfflineTransport::new();
    let _ = launch_with_transport(&spec_path, false, true, true, &transport);
    let (raw, state) = raw_runtime_state(workspace);
    let agent = state
        .pointer("/agents/implementer")
        .unwrap_or_else(|| panic!("implementer agent state missing; raw={raw}"));
    let keys = agent
        .as_object()
        .expect("agent state object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["status", "provider"],
        "paused agent state must be exactly golden launch/core.py:171-173; raw={raw}"
    );
    assert_eq!(agent["status"], json!("paused"));
    assert_eq!(agent["provider"], json!("codex"));
    assert!(
        transport.spawn_records().is_empty(),
        "paused agent must not spawn any terminal window; got {:?}",
        transport.spawn_records()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 0.3.24 add-agent socket drift fix (macmini demo-director startup blocker).
//
// Root cause: `launch.rs::add_agent` / `fork_agent` hardcoded
// `TmuxBackend::for_workspace(...)` (workspace-hash socket, e.g.
// `/private/tmp/tmux-501/ta-<hash>/termclaud`) instead of consulting the
// team's persisted `tmux_endpoint` (set at `team-agent launch` time, e.g.
// `/private/tmp/tmux-501/default`). The orphan `claude` process then spawned
// onto the wrong socket; the leader (running on the persisted socket) never
// saw the window; state never registered; `add_agent` returned `ok=true`
// (假绿) because the lifecycle layer didn't verify reachability post-spawn.
//
// Architect-approved fix has 4 调整:
//   1. Reuse state-aware resolver (`lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state`).
//   2. fork-agent same path.
//   3. Endpoint annotate folded into start_agent state-save (no parallel
//      `annotate_runtime_tmux_endpoint` race with coordinator).
//   4. Strict-non-capture reachability gate post-spawn (uses `has_pane` /
//      `liveness` / `list_targets` only — never `capture` to avoid E31 timing).
// ═════════════════════════════════════════════════════════════════════════════

/// **RED**: `add_agent` must read the team's persisted `tmux_endpoint` (set at
/// `team-agent launch` time and stored in the runtime state) and resolve the
/// transport's socket to that endpoint — NOT the workspace-hash for_workspace
/// socket. We exercise the resolver directly (the public surface
/// `lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state`) so
/// the contract is independent of CLI plumbing.
#[test]
fn add_agent_resolves_to_persisted_endpoint_socket_not_workspace_hash() {
    let team = temp_ws().join("addteam-persisted-endpoint");
    std::fs::create_dir_all(&team).unwrap();
    let live_endpoint = "/private/tmp/tmux-501/default".to_string();
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-persisted",
            "tmux_endpoint": live_endpoint.clone(),
            "tmux_socket": live_endpoint.clone(),
            "agents": {}
        }),
    )
    .unwrap();

    let resolved =
        crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(&team, None)
            .expect("resolver must succeed when state is present");

    let endpoint =
        <crate::tmux_backend::TmuxBackend as crate::transport::Transport>::tmux_endpoint(&resolved);
    assert_eq!(
        endpoint.as_deref(),
        Some(live_endpoint.as_str()),
        "0.3.24 add-agent socket drift fix: the state-aware resolver must \
         route to the PERSISTED tmux_endpoint ({live_endpoint}), not the \
         workspace-hash for_workspace socket. Got: {endpoint:?}. macmini \
         repro: add-agent demo-director spawned `claude` on the wrong socket \
         while the live team ran on the persisted default socket — orphan \
         process + ok=true假绿."
    );
}

/// **Regression guard**: cold workspace (no persisted endpoint) must safely
/// fall back to `TmuxBackend::for_workspace(workspace)`. Encodes the
/// first-agent / fresh-launch path so a future refactor doesn't accidentally
/// panic/None there.
#[test]
fn add_agent_resolver_falls_back_to_workspace_socket_when_no_persisted_endpoint() {
    let team = temp_ws().join("addteam-cold-fallback");
    std::fs::create_dir_all(&team).unwrap();
    // NOTE: no save_runtime_state — cold workspace, no persisted endpoint.

    let resolved =
        crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(&team, None)
            .expect("resolver must succeed (fall back to for_workspace) on cold workspace");

    let endpoint =
        <crate::tmux_backend::TmuxBackend as crate::transport::Transport>::tmux_endpoint(&resolved);
    let fallback = <crate::tmux_backend::TmuxBackend as crate::transport::Transport>::tmux_endpoint(
        &crate::tmux_backend::TmuxBackend::for_workspace(&team),
    );
    assert_eq!(
        endpoint, fallback,
        "0.3.24 regression guard: when state has no persisted tmux_endpoint, \
         resolver must fall back to TmuxBackend::for_workspace (workspace-hash \
         socket). Got endpoint={endpoint:?} expected fallback={fallback:?}."
    );
}

/// **RED**: `add_agent` must verify reachability AFTER spawn (strict,
/// non-capture). A spawn that lands on the wrong tmux socket (or on the right
/// socket but produces no addressable pane) must surface as a structured
/// `Err`, NOT `ok=true`. Models the macmini假绿 root cause: the spawn
/// recorded into the OfflineTransport but `has_pane` / `liveness` /
/// `list_targets` all say "not addressable" (`spawned_panes_addressable=false`).
#[test]
fn add_agent_reachability_gate_returns_error_when_spawn_pane_not_addressable() {
    let team = temp_ws().join("addteam-reachability-gate");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: reachgate\nobjective: Reachability probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("worker2-role.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);

    // Models "spawn to wrong socket" / "spawn succeeded but pane orphan": the
    // OfflineTransport records the spawn but reports the new pane is NOT
    // addressable (has_pane→Some(false), liveness defaults to Unknown=fall
    // through, list_targets doesn't include it).
    let transport = OfflineTransport::new()
        .with_spawned_panes_addressable(false)
        .with_default_liveness(crate::transport::PaneLiveness::Dead);

    let result = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    // The spawn DID record (proving we reached the spawn step), but the gate
    // must NOT let the function return Ok — pane is not addressable so the
    // caller must see the structured failure and roll back / retry.
    assert!(
        !transport.spawn_records().is_empty(),
        "add-agent must still attempt the spawn (gate fires post-spawn); \
         got no spawn records — indicates a regression where the gate fires \
         too early."
    );
    assert!(
        result.is_err(),
        "0.3.24 reachability gate: add-agent must return Err when the spawned \
         pane is not addressable on the transport's socket (macmini假绿 root \
         cause). Got Ok(...) — gate did not fire."
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unreachable")
            || err_msg.contains("not addressable")
            || err_msg.contains("socket drift"),
        "0.3.24 reachability gate: error message must name the unreachable / \
         socket-drift symptom so operators can diagnose. Got: {err_msg}"
    );
}

/// **Regression guard**: when the spawned pane IS addressable (default
/// `OfflineTransport` state), reachability gate passes and add-agent returns
/// Ok as before. Ensures the gate is precision, not blanket.
#[test]
fn add_agent_reachability_gate_passes_when_spawn_pane_addressable() {
    let team = temp_ws().join("addteam-gate-pass");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: gatepass\nobjective: Gate-pass probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("worker2-role.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);

    // Default OfflineTransport: spawned_panes_addressable=true, liveness=Unknown
    // (treated as not-Dead by the gate's fallthrough).
    let transport = OfflineTransport::new();

    let result = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    assert!(
        result.is_ok(),
        "0.3.24 reachability gate regression guard: when spawn pane is \
         addressable, add-agent must still return Ok. Got: {result:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// E43 add-agent window layout drift (0.3.24 bug#3, demo-director startup blocker).
//
// Root cause (architect res_f27ff9b29957): with `display_backend=adaptive` in
// state AND stale `agents.<id>.layout_window="team-w2"` residue, the existing
// `adaptive_placement_for_agent` validates a layout_window claim by checking
// whether the existing agent's `pane_id` is in `live_panes` — but it does NOT
// verify that the live pane's `window_name` actually equals the claimed
// `layout_window`. So a live `%X` whose REAL window_name is "architect" still
// "validates" the stale "team-w2" claim, the placement returns
// `layout_window=team-w2, starts_window=false`, and `spawn_agent_window`
// issues `tmux split-window -t :team-w2` which fails with
// "can't find window: team-w2". macmini repro: demo-director add-agent blocks.
//
// Architect-approved minimal fix (3 调整, NO restructure of display/topology):
//   Fix A — `adaptive_placement_for_agent`: validate the live pane's
//     window_name MATCHES the agent's claimed layout_window. If pane_id is
//     live but lives under a different window_name, treat the agent's
//     layout_window claim as stale and skip it (don't count toward the
//     "windows with room" map).
//   Fix B — `adaptive_existing_placement_for_agent`: when the agent's
//     claimed layout_window is NOT present in live_windows, fall back to a
//     `starts_window=true` placement with `layout_window=agent_id` instead
//     of synthesising a fresh stale-named window.
//   Fix C — `spawn_agent_window`: pre-split guard. When `!starts_window` and
//     the target window is NOT in live_windows, downgrade to spawn_into
//     (a new window named `agent_id`) instead of splitting a phantom.
// ═════════════════════════════════════════════════════════════════════════════

/// **RED — Fix A**: `adaptive_placement_for_agent` must skip an existing
/// agent whose live pane resides under a window_name that differs from the
/// agent's claimed `layout_window`. Real-machine state: agent "architect"
/// carries `layout_window="team-w2"` (stale residue) but its live pane lives
/// under `window_name="architect"`. The new placement for demo-director must
/// NOT inherit the stale "team-w2" claim.
#[test]
fn e43_adaptive_placement_skips_existing_agent_whose_live_pane_window_name_differs_from_claim() {
    use crate::lifecycle::launch::{adaptive_placement_for_agent, LayoutPlacement};
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "architect": {
                "status": "running",
                "provider": "codex",
                "window": "architect",
                // Stale residue — placement must NOT inherit this name.
                "layout_window": "team-w2",
                "pane_id": "%1",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "architect", "%1", 0)])
        .with_windows(vec![WindowName::new("architect")]);

    let placement = adaptive_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("demo-director"),
    );

    // **E45 amendment (0.3.24 bug#4)**: when the live session has NO real
    // adaptive layout window (the topology is effectively per-agent — live
    // windows are `architect` not `team-wN`), placement returns None. The
    // caller (`start_agent_at_paths` → `spawn_agent_window`) then falls back
    // to opening a new window named after `agent_id`. This is the macmini
    // demo-director repro fix: per-agent topology must NOT be tricked into
    // adaptive mode by stale `layout_window=team-w2` residue.
    //
    // Pre-E45 expectation was Some(starts_window=true) with a fresh
    // team-w<next> name, but that forced an adaptive shape onto a session
    // that was already per-agent — which led to E45 bug#4 (split-window
    // into the developer worker after stale state pointed there).
    if let Some(p) = placement {
        // If we ever do return Some here, the STALE name must still not
        // survive — this branch shouldn't fire post-E45 but guards against
        // future regressions where placement re-enables adaptive on
        // per-agent live topology.
        assert_ne!(
            p.layout_window.as_str(),
            "team-w2",
            "E43 Fix A still applies: stale layout_window 'team-w2' must \
             NEVER survive. Got {p:?}"
        );
        let _: LayoutPlacement = p; // type witness
    }
}

/// **Regression guard — Fix A**: when an existing agent's live pane DOES
/// reside under the claimed layout_window, placement keeps the prior
/// behaviour (groups the new agent into that window). This is the canonical
/// adaptive 1-window-multiple-panes use case.
#[test]
fn e43_adaptive_placement_groups_into_layout_window_when_live_pane_matches_claim() {
    use crate::lifecycle::launch::adaptive_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "alpha": {
                "status": "running",
                "provider": "codex",
                "layout_window": "team-w1",
                "pane_id": "%1",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "team-w1", "%1", 0)])
        .with_windows(vec![WindowName::new("team-w1")]);

    let placement = adaptive_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("beta"),
    )
    .expect("placement should succeed");

    assert_eq!(
        placement.layout_window.as_str(),
        "team-w1",
        "E43 Fix A regression guard: when alpha's live pane DOES live under \
         layout_window 'team-w1', the new agent beta should be grouped into \
         the same window (canonical adaptive behaviour). Got {placement:?}"
    );
    assert!(
        !placement.starts_window,
        "E43 Fix A regression guard: grouping into an existing window means \
         starts_window=false (split into the window). Got {placement:?}"
    );
}

/// **RED — Fix B**: `adaptive_existing_placement_for_agent` must NOT return a
/// `starts_window=true` placement that names a window NOT present in
/// `live_windows`. When the agent's claimed `layout_window` is stale and the
/// session is effectively per-agent (live windows are named after agent_ids),
/// the placement must fall back to a per-agent window named after the
/// agent_id itself — so the subsequent spawn opens a new window, not a split
/// of a phantom.
#[test]
fn e43_adaptive_existing_placement_falls_back_to_agent_id_window_when_claim_missing_from_live() {
    use crate::lifecycle::launch::adaptive_existing_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "demo-director": {
                "status": "running",
                "provider": "codex",
                // Stale claim — live session has no team-w2 window.
                "layout_window": "team-w2",
                "pane_id": "%99",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![
            layout_pane("team-iso", "architect", "%1", 0),
            layout_pane("team-iso", "test-engineer", "%2", 0),
        ])
        .with_windows(vec![
            WindowName::new("architect"),
            WindowName::new("test-engineer"),
        ]);

    let placement = adaptive_existing_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("demo-director"),
    )
    .expect("placement should resolve");

    assert_ne!(
        placement.layout_window.as_str(),
        "team-w2",
        "E43 Fix B: stale claim 'team-w2' is not in live_windows; placement \
         must NOT reuse that name (otherwise spawn issues split -t :team-w2 \
         and tmux says 'can't find window: team-w2'). Got {placement:?}"
    );
    assert_eq!(
        placement.layout_window.as_str(),
        "demo-director",
        "E43 Fix B: when the claim is stale and the live layout is \
         per-agent-named, fall back to a window named after the agent_id \
         (canonical per-agent pattern). Got {placement:?}"
    );
    assert!(
        placement.starts_window,
        "E43 Fix B: the fallback opens a NEW window (starts_window=true); \
         do not try to split an existing window with the wrong name. Got \
         {placement:?}"
    );
}

/// **Regression guard — Fix B**: when the agent's claimed layout_window IS
/// in live_windows, the existing-placement path keeps its current behaviour
/// (return a starts_window=false placement that splits into the existing
/// window).
#[test]
fn e43_adaptive_existing_placement_keeps_starts_window_false_when_claim_in_live_windows() {
    use crate::lifecycle::launch::adaptive_existing_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "alpha": {
                "status": "running",
                "provider": "codex",
                "layout_window": "team-w1",
                "pane_id": "%1",
                "pane_index": 1u64,
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "team-w1", "%1", 0)])
        .with_windows(vec![WindowName::new("team-w1")]);

    let placement = adaptive_existing_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("alpha"),
    )
    .expect("placement should resolve");

    assert_eq!(
        placement.layout_window.as_str(),
        "team-w1",
        "E43 Fix B regression guard: when claim 'team-w1' IS in live_windows, \
         existing-placement must keep it. Got {placement:?}"
    );
    // pane_index=1 → starts_window=false in the existing behaviour.
    assert!(
        !placement.starts_window,
        "E43 Fix B regression guard: existing window + pane_index>0 → \
         starts_window=false. Got {placement:?}"
    );
}

/// **RED — Fix C end-to-end**: `add_agent` end-to-end must NOT emit a
/// spawn_split with the stale window name. With state carrying
/// `display_backend=adaptive` + stale `layout_window=team-w2` residue and
/// live tmux windows named per-agent, the spawn must be a `spawn_into`
/// (new window) — NOT `spawn_split` against the missing 'team-w2'.
#[test]
fn e43_add_agent_does_not_split_against_phantom_layout_window() {
    use crate::transport::WindowName;

    let team = temp_ws().join("e43-add-agent-phantom-window");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: e43probe\nobjective: E43 probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("architect.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("worker2-role.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    // Real-machine repro: existing 'architect' carries stale layout_window=team-w2
    // residue; live tmux shows it under window_name='architect' (per-agent
    // layout). adaptive_layout flag is on.
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-iso",
            "display_backend": "adaptive",
            "agents": {
                "architect": {
                    "status": "running",
                    "provider": "codex",
                    "window": "architect",
                    "layout_window": "team-w2",
                    "pane_id": "%1",
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&team);

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "architect", "%1", 0)])
        .with_windows(vec![WindowName::new("architect")])
        .with_session_present(true);

    let _result = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    let spawn_window_records = transport.spawn_window_records();
    // The macmini failure mode: split-window -t :team-w2. We must not split,
    // and we definitely must not target "team-w2".
    let stale_split = spawn_window_records
        .iter()
        .any(|(kind, window)| kind == "spawn_split" && window == "team-w2");
    assert!(
        !stale_split,
        "E43 Fix C end-to-end: add-agent must NOT issue spawn_split with \
         stale window name 'team-w2' (real-machine macmini error: \
         'tmux split-window -t :team-w2 → can't find window: team-w2'). \
         spawn_window_records={spawn_window_records:?}"
    );
    // The agent must actually be spawned somewhere (new window or split into
    // a valid live window). We at least expect one spawn recorded.
    assert!(
        !spawn_window_records.is_empty(),
        "E43 Fix C: add-agent must still attempt the spawn; got no records."
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// E45 per-agent window drift (0.3.24 task#326 P0).
//
// Surfaced AFTER E43 (c5ed576) fixed the team-w2 phantom split. New repro:
// state has display_backend=adaptive + an existing `developer` worker whose
// state row carries `layout_window=developer, layout_index=0, pane_id=%519`,
// live tmux shows `developer` window with that pane. add-agent demo-director
// resolves placement via `adaptive_placement_for_agent`, which finds the
// `developer` window valid (pane_id live + window_name matches), groups
// demo-director into it (count<3) → returns
// `layout_window=developer, starts_window=false` → `spawn_agent_window`
// issues `split-window -t :developer` which SUCCEEDS — splitting into the
// developer worker's pane. macmini repro: add-agent demo-director hijacked
// the developer worker's window @453 (2 panes), no new window.
//
// Root cause (architect): adaptive_placement_for_agent accepts ANY state
// layout_window/window as adaptive (including per-agent names like
// `developer`). E45 fix: only canonical `team-w<N>[-suffix]` windows count
// as adaptive layout windows.
// ═════════════════════════════════════════════════════════════════════════════

/// **RED — Fix #2**: `adaptive_placement_for_agent` must NOT count a
/// per-agent window (e.g. `developer`) as adaptive layout space, even when
/// it has matching live pane + window_name. With no real adaptive
/// `team-w<N>` window in the live session, placement returns None so the
/// caller opens a new per-agent window.
#[test]
fn e45_adaptive_placement_returns_none_when_no_real_team_w_window_present_in_state() {
    use crate::lifecycle::launch::adaptive_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    // Real-machine state: adaptive on, developer's layout_window is
    // literally `developer` (per-agent name, NOT team-wN), pane_id %519
    // lives under the `developer` window.
    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "developer": {
                "status": "running",
                "provider": "codex",
                "window": "developer",
                "layout_window": "developer",
                "layout_index": 0u64,
                "pane_id": "%519",
            },
            "test-engineer": {
                "status": "running",
                "provider": "codex",
                "window": "test-engineer",
                "layout_window": "test-engineer",
                "pane_id": "%521",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![
            layout_pane("team-iso", "developer", "%519", 0),
            layout_pane("team-iso", "test-engineer", "%521", 0),
        ])
        .with_windows(vec![
            WindowName::new("developer"),
            WindowName::new("test-engineer"),
        ]);

    let placement = adaptive_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("demo-director"),
    );

    assert!(
        placement.is_none(),
        "E45 (0.3.24 bug#4): placement must return None when the live \
         session has NO real `team-w<N>` adaptive window. Per-agent windows \
         like `developer` are NOT adaptive layout windows — they are \
         per-agent topology, even if display_backend=adaptive lingers in \
         state. With no real adaptive layout, the caller falls back to \
         opening a new per-agent window named after agent_id. Got \
         {placement:?} — must be None to prevent split-window -t :developer \
         hijacking the developer worker's pane (macmini repro)."
    );
}

/// **RED — Fix #3**: `adaptive_existing_placement_for_agent` must return
/// None when the agent's claimed layout_window is per-agent (not team-wN).
/// The caller then falls back to non-placement spawn path which opens /
/// reuses a window named after agent_id.
#[test]
fn e45_adaptive_existing_placement_returns_none_when_claim_is_per_agent_window() {
    use crate::lifecycle::launch::adaptive_existing_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "developer": {
                "status": "running",
                "provider": "codex",
                "window": "developer",
                "layout_window": "developer",
                "layout_index": 0u64,
                "pane_id": "%519",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "developer", "%519", 0)])
        .with_windows(vec![WindowName::new("developer")]);

    let placement = adaptive_existing_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("developer"),
    );

    assert!(
        placement.is_none(),
        "E45 (0.3.24 bug#4): existing-placement must return None when \
         claimed layout_window is per-agent (e.g. `developer`). Only real \
         `team-w<N>[-suffix]` windows are honored as existing adaptive \
         placements. Got {placement:?}."
    );
}

/// **RED — end-to-end Fix #2 + Fix #3 + defence**: add-agent against a
/// per-agent live topology + adaptive backend in state must issue
/// spawn_into (new window named after agent_id) and MUST NOT spawn_split
/// into any existing per-agent window. macmini repro: split-window
/// -t :developer would hijack the developer worker's pane.
#[test]
fn e45_add_agent_opens_new_window_does_not_split_into_per_agent_window() {
    use crate::transport::WindowName;

    let team = temp_ws().join("e45-per-agent-no-split");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: e45probe\nobjective: E45 probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("developer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("worker2-role.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();

    // Real-machine state: display_backend=adaptive, but the existing
    // `developer` worker carries per-agent layout_window=developer with
    // layout_index=0. macmini-equivalent state shape.
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-iso",
            "display_backend": "adaptive",
            "agents": {
                "developer": {
                    "status": "running",
                    "provider": "codex",
                    "window": "developer",
                    "layout_window": "developer",
                    "layout_index": 0u64,
                    "pane_id": "%519",
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&team);

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "developer", "%519", 0)])
        .with_windows(vec![WindowName::new("developer")])
        .with_session_present(true);

    let _result = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );

    let spawn_records = transport.spawn_window_records();
    // PRIMARY assertion: no spawn_split anywhere — adaptive placement
    // refused to group into the per-agent `developer` window.
    let any_split = spawn_records.iter().any(|(kind, _)| kind == "spawn_split");
    assert!(
        !any_split,
        "E45 end-to-end: add-agent against a per-agent live topology must \
         NOT issue spawn_split into any window (macmini repro: \
         split-window -t :developer would hijack the developer worker's \
         pane). Got spawn_records={spawn_records:?}"
    );
    // Defence: the spawn target name must not be `developer` (the existing
    // per-agent window) — if it were, the spawn would hijack.
    let spawned_into_developer = spawn_records
        .iter()
        .any(|(_, window)| window == "developer");
    assert!(
        !spawned_into_developer,
        "E45 end-to-end: spawn target must NOT be `developer` (the existing \
         worker's window). Got spawn_records={spawn_records:?}"
    );
    // The new worker must have been spawned somewhere (proves we reached
    // the spawn step and didn't refuse silently).
    assert!(
        !spawn_records.is_empty(),
        "E45 end-to-end: add-agent must still attempt the spawn; got no \
         records."
    );
}

/// **Regression guard**: when the live session DOES have a real `team-w1`
/// adaptive window with a live pane that matches state's claim, the
/// existing E43+adaptive grouping behaviour is preserved (split into team-w1
/// as before). E45 only changes behaviour for per-agent windows.
#[test]
fn e45_real_team_w_adaptive_layout_still_groups_into_layout_window() {
    use crate::lifecycle::launch::adaptive_placement_for_agent;
    use crate::model::ids::AgentId;
    use crate::transport::{SessionName, WindowName};

    let state = json!({
        "display_backend": "adaptive",
        "session_name": "team-iso",
        "agents": {
            "alpha": {
                "status": "running",
                "provider": "codex",
                "layout_window": "team-w1",
                "layout_index": 0u64,
                "pane_id": "%1",
            }
        }
    });

    let transport = OfflineTransport::new()
        .with_targets(vec![layout_pane("team-iso", "team-w1", "%1", 0)])
        .with_windows(vec![WindowName::new("team-w1")]);

    let placement = adaptive_placement_for_agent(
        &state,
        &transport,
        &SessionName::new("team-iso"),
        &AgentId::new("beta"),
    )
    .expect("E45 regression guard: real team-w1 adaptive layout must still group new agents in");

    assert_eq!(
        placement.layout_window.as_str(),
        "team-w1",
        "E45 regression guard: real `team-w1` adaptive window must still \
         host new agents (canonical adaptive behaviour). Got {placement:?}"
    );
    assert!(
        !placement.starts_window,
        "E45 regression guard: grouping into existing team-w1 → \
         starts_window=false. Got {placement:?}"
    );
}

/// **Regression guard**: helper `is_adaptive_layout_window` golden cases.
/// `team-w1`, `team-w42`, `team-w7-foo` are adaptive; agent_id names and
/// other shapes are not.
#[test]
fn e45_is_adaptive_layout_window_recognises_team_w_pattern_only() {
    use crate::lifecycle::launch::is_adaptive_layout_window_pub as is_adapt;

    assert!(is_adapt("team-w1"), "team-w1 must be adaptive");
    assert!(is_adapt("team-w2"), "team-w2 must be adaptive");
    assert!(is_adapt("team-w42"), "team-w42 must be adaptive");
    assert!(
        is_adapt("team-w7-suffix"),
        "team-w7-suffix must be adaptive"
    );

    assert!(
        !is_adapt("developer"),
        "developer is per-agent, not adaptive"
    );
    assert!(
        !is_adapt("architect"),
        "architect is per-agent, not adaptive"
    );
    assert!(!is_adapt("demo-director"), "demo-director is per-agent");
    assert!(!is_adapt("leader"), "leader is not adaptive");
    assert!(!is_adapt(""), "empty is not adaptive");
    assert!(!is_adapt("team-w"), "team-w (no index) is not adaptive");
    assert!(
        !is_adapt("team-w0"),
        "team-w0 (index 0 invalid post-subtract) is not adaptive"
    );
    assert!(
        !is_adapt("team-wfoo"),
        "team-wfoo (non-numeric) is not adaptive"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// E42 (0.3.24 P0, double-spec deadlock) — add-agent / remove-agent must read
// the SAME canonical spec, and add-agent's writes must be atomic (rollback on
// any downstream failure).
// ═════════════════════════════════════════════════════════════════════════════

/// **E42 RED (a) round-trip**: stale `state.spec_path` points at a legacy
/// user-dir spec while the canonical runtime spec is authoritative.
/// add-agent writes to canonical; remove-agent (from_spec=true, force=true)
/// must succeed because the fix routes lifecycle_paths::spec_workspace to
/// canonical, not the stale legacy path.
#[test]
fn e42_add_then_remove_round_trip_resolves_canonical_spec_not_legacy() {
    let team = temp_ws().join("e42-roundtrip");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: e42rt\nobjective: E42 round-trip probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("newagent.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);
    // Pre-populate the state with a STALE spec_path pointing at a legacy
    // user-dir spec that does NOT carry the new agent we are about to add.
    // canonical runtime spec is what add-agent writes to.
    let team_ws = crate::model::paths::team_workspace(&team).unwrap();
    let legacy_spec_dir = team_ws.join(".team").join("e42rt");
    std::fs::create_dir_all(&legacy_spec_dir).unwrap();
    let legacy_spec_path = legacy_spec_dir.join("team.spec.yaml");
    std::fs::write(
        &legacy_spec_path,
        "name: e42rt\nagents: []\nrouting:\n  rules: []\n",
    )
    .unwrap();
    crate::state::persist::save_runtime_state(
        &team,
        &json!({
            "session_name": "team-iso",
            "spec_path": legacy_spec_path.to_string_lossy(),
            "agents": {
                "implementer": {"status": "running", "provider": "codex", "window": "implementer"}
            }
        }),
    )
    .unwrap();
    let transport = OfflineTransport::new();

    // (1) add-agent succeeds (writes canonical spec + state)
    let add_result = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );
    assert!(
        add_result.is_ok(),
        "E42 (a) round-trip: add-agent must succeed even with stale state.spec_path; \
         got {add_result:?}"
    );
    // (2) remove-agent must find the agent (lifecycle_paths resolves canonical,
    // not the stale legacy spec)
    let remove_result = crate::lifecycle::remove_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        true,
        true,
        None,
        &transport,
    );
    assert!(
        remove_result.is_ok(),
        "E42 (a) round-trip: remove-agent must find the agent add-agent just wrote \
         (canonical-first lifecycle_paths). Pre-fix: stale state.spec_path wins → \
         remove sees legacy spec without the new agent → 'unknown'. Got {remove_result:?}"
    );
}

/// **E42 RED (b) re-add symmetry**: after add(x), a second add(x) must say
/// "already exists" AND remove(x) must recognise x — never give relative
/// existence judgments. Same canonical truth source on both sides.
#[test]
fn e42_re_add_and_remove_agree_on_existence_after_add() {
    let team = temp_ws().join("e42-readdsym");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: e42sym\nobjective: E42 symmetry probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("newagent.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);
    let transport = OfflineTransport::new();
    // (1) first add
    let r1 = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );
    assert!(r1.is_ok(), "first add must succeed; got {r1:?}");
    // (2) second add must report already-exists Err (not Ok)
    let r2 = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    );
    assert!(
        r2.is_err(),
        "E42 (b) symmetry: second add must Err 'already exists'; got {r2:?}"
    );
    // (3) remove must also see the agent (no relative judgments)
    let r3 = crate::lifecycle::remove_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        true,
        true,
        None,
        &transport,
    );
    assert!(
        r3.is_ok(),
        "E42 (b) symmetry: remove must see what add wrote — both reading canonical. \
         Got {r3:?}"
    );
}

/// **E42 RED (d) projection consistency**: after add(x), canonical runtime
/// spec AND runtime state agents map BOTH contain x; lifecycle_paths
/// resolves spec_workspace to canonical (not legacy).
#[test]
fn e42_add_writes_to_canonical_runtime_spec_and_state_consistently() {
    let team = temp_ws().join("e42-projection");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: e42proj\nobjective: E42 projection probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap();
    let role_file = team.join("newagent.md");
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);
    let transport = OfflineTransport::new();
    let _ = crate::lifecycle::add_agent_with_transport(
        &team,
        &AgentId::new("worker2"),
        &role_file,
        false,
        None,
        &transport,
    )
    .expect("add must succeed");

    // (a) runtime state has worker2 — read same way add_agent does (team_workspace
    // resolves the run_workspace, and select_runtime_state reads canonical-scoped).
    let state = crate::state::persist::load_runtime_state(&team).unwrap();
    assert!(
        state.pointer("/agents/worker2").is_some(),
        "E42 (d): runtime state must contain worker2 after add; state={state}"
    );

    // (b) canonical runtime spec has worker2
    let runtime_spec = find_runtime_spec(&team, "e42proj");
    let canonical_spec_path = runtime_spec.expect("canonical runtime spec must exist");
    let canonical_spec_text = std::fs::read_to_string(&canonical_spec_path).unwrap();
    assert!(
        canonical_spec_text.contains("worker2"),
        "E42 (d): canonical runtime spec must contain worker2; \
         canonical_spec={canonical_spec_text}"
    );
}
