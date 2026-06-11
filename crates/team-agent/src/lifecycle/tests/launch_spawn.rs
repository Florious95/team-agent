use super::*;
use crate::transport::test_support::OfflineTransport;
use serial_test::serial;

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
    let result = quick_start_with_transport(&team, None, true, true, None, &transport);

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
    let result = quick_start_with_transport(&team, None, true, true, None, &transport);
    assert!(matches!(result, Ok(QuickStartReport::Ready { .. })), "quick-start failed: {result:?}");

    let state_path = crate::state::persist::runtime_state_path(&workspace);
    assert!(state_path.exists(), "quick-start .team/current must persist runtime state under project root");
    assert!(
        !workspace.join(".team").join(".team").join("runtime").join("state.json").exists(),
        "quick-start .team/current must not create nested .team/.team runtime state"
    );

    for input in [&workspace, &team] {
        let selected = crate::state::selector::resolve_active_team(
            input,
            None,
            crate::state::selector::SelectorMode::RuntimeOnly,
        )
        .expect("status/collect selector should resolve project root");
        assert_eq!(selected.run_workspace, workspace, "input={}", input.display());
        // E5: selector resolves spec_path to .team/runtime/<team_key>/ (team_key="current").
        let expected_spec = crate::model::paths::runtime_spec_path(&workspace, "current");
        assert_eq!(
            selected.spec_path.as_deref().map(std::fs::canonicalize).transpose().unwrap(),
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
    });
    assert!(status.is_ok(), "status should normalize teamdir to project-root runtime state: {status:?}");

    let collect = crate::cli::cmd_collect(&crate::cli::CollectArgs {
        result_file: None,
        workspace: team.clone(),
        json: true,
    });
    assert!(collect.is_ok(), "collect should normalize teamdir to the same project-root state/spec: {collect:?}");
}

// P0 — quick_start over an INVALID role doc (missing `provider`) must surface the REAL compile
// error, distinct from the stub's hardcoded "no role docs found". Golden: compile_team raises
// "missing front matter field provider" (compiler.py:_validate_role_doc), before preflight.
#[test]
fn quick_start_invalid_role_doc_surfaces_real_compile_error() {
    let team = quick_start_team_dir(QS_ROLE_NO_PROVIDER);
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport(&team, None, true, true, None, &transport);

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
    let result = start_agent_with_transport(&ws, &AgentId::new("w1"), false, false, true, None, &transport);
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
    let report = quick_start(&team, None, true, true, None).expect("quick_start launches the team");
    assert!(matches!(report, QuickStartReport::Ready { .. }), "got {report:?}");
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
    let first = agents.first_mut().expect("compiled spec must contain an agent");
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
    let _ = quick_start_with_transport(&team, None, true, true, None, &transport);
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
    assert!(report.safety.enabled, "safety must reflect spec.runtime.dangerous_auto_approve=true");
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

// #14 — add_agent rejects a duplicate agent id (golden operations.py:301 'agent id already exists'),
// BEFORE any full recompile. Current does a full compile_team with no duplicate-id guard.
#[test]
fn spine_add_agent_rejects_duplicate_agent_id() {
    let team = quick_start_team_dir(QS_VALID_ROLE); // team already has agent 'implementer'
    let role = team.join("dup-role.md");
    std::fs::write(&role, QS_VALID_ROLE).unwrap(); // a role doc named 'implementer' = duplicate
    let transport = OfflineTransport::new();
    let result = add_agent_with_transport(&team, &AgentId::new("implementer"), &role, false, None, &transport);
    let text = format!("{result:?}");
    assert!(
        text.contains("already exists"),
        "add_agent must reject a duplicate agent id (golden 'agent id already exists'); got {text}"
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
    assert!(!ws.join("team.spec.yaml").exists(), "user-dir spec removed (spec-demote condition)");
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
        text.contains("missing spec") || text.to_lowercase().contains("not found") || text.contains("no_local_team_context") || text.to_lowercase().contains("invalid workspace"),
        "RED-2 negative: empty workspace restart must still error (missing/not-found); got {text}"
    );
    let _ = std::fs::remove_dir_all(&ws);
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
    let result = add_agent_with_transport(&team, &AgentId::new("w2"), &role, false, None, &transport);
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
    assert!(!report.started.is_empty(), "a real (non-dry-run) launch must spawn >=1 worker");
    assert!(!report.dry_run, "launch_with_transport(dry_run=false) must report dry_run=false");
    assert_eq!(recorded[0].0, "spawn_first", "the first worker uses new-session (spawn_first)");
    assert!(
        recorded.iter().any(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "each spawn argv must carry the agent's provider build_command (codex); got {recorded:?}"
    );
    assert!(
        report.started.iter().any(|s| s.agent_id.as_str() == "implementer"),
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
    crate::coordinator::write_coordinator_metadata(&wp, me, crate::coordinator::MetadataSource::Boot).unwrap();
    std::fs::write(crate::coordinator::coordinator_pid_path(&wp), me.to_string()).unwrap();
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

    let report = quick_start_with_transport(&team, None, true, true, None, &transport)
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
        launch.started.iter().any(|s| s.agent_id.as_str() == "implementer"),
        "launch.started must list the compiled agent 'implementer'; got {:?}",
        launch.started
    );
    // (3) the transport actually recorded the spawn, carrying the provider build_command.
    let recorded = transport.spawn_records();
    assert!(
        !recorded.is_empty(),
        "quick_start must drive the transport spawn path (>=1 spawn recorded); the dry-run bug records ZERO spawns"
    );
    assert_eq!(recorded[0].0, "spawn_first", "the first worker uses new-session (spawn_first); got {recorded:?}");
    assert!(
        recorded.iter().any(|(_, argv)| argv.iter().any(|a| a == "codex")),
        "the spawn argv must carry the agent's provider build_command (codex); got {recorded:?}"
    );
}

// REAL-MACHINE residency boundary (acceptance framework): the PUBLIC quick_start (real TmuxBackend +
// real start_coordinator) on a fresh ws must leave a LIVE tmux session AND a ps-verifiable resident
// coordinator daemon (start_coordinator -> live pid). The framework verifies residency via ps; here we
// only assert the in-report spawn observable. #[ignore] — spawns real tmux + a real daemon subprocess.
#[test]
#[ignore = "real-machine: live tmux session + resident coordinator daemon (framework verifies via ps)"]
fn quick_start_fresh_ws_spawns_resident_tmux_and_coordinator() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let report = quick_start(&team, None, true, true, None).expect("quick_start");
    match report {
        QuickStartReport::Ready { launch, .. } => {
            assert!(!launch.dry_run, "a real quick_start must spawn (not dry-run)");
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
    std::fs::write(ws.join("TEAM.md"), "---\nname: restartteam\nobjective: Restart probe.\nprovider: codex\n---\n\nteam.\n").unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile 2-agent team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-restartteam",
            "agents": {
                "alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "first_send_at": "2026-05-27T10:00:00+00:00"},
                "bravo": {"status": "running", "provider": "codex", "session_id": "sess-b", "first_send_at": "2026-05-27T10:00:00+00:00"}
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws); // start_coordinator -> AlreadyRunning (no real daemon subprocess)
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

    let ws = restart_ws_two_resumable_workers();
    let dead_after_first = OfflineTransport::new().with_session_absent_after_spawn_first();
    let result = restart_with_transport(&ws, false, None, &dead_after_first);
    let recorded = dead_after_first.spawn_records();
    assert_eq!(
        recorded.len(),
        1,
        "if the session disappears after alpha, restart must stop before spawning bravo; got result={result:?} recorded={recorded:?}"
    );
    assert!(
        recorded.iter().all(|(kind, _)| kind != "spawn_into"),
        "restart must not call spawn_into/new-window for bravo against a dead server; result={result:?} recorded={recorded:?}"
    );
    let error = format!("{result:?}");
    assert!(
        error.contains("session_disappeared_after_spawn") || error.contains("provider_resume_exited"),
        "restart must report the first resumed agent/session disappearance explicitly, not continue to a later no-server failure; result={result:?}"
    );
}

// 3 [P0] — start_agent_with_transport on a non-paused agent with a session_id must spawn EXACTLY ONE
// worker (resume) carrying the provider build_command. Today the stub returns RequirementUnmet with
// ZERO spawns -> RED at recorded.len().
#[test]
fn start_agent_with_transport_spawns_resume_not_stub() {
    let ws = temp_ws().join("startagentws");
    std::fs::create_dir_all(&ws).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-sa",
            "agents": {"alpha": {"status": "running", "provider": "codex", "session_id": "sess-a", "first_send_at": "2026-05-27T10:00:00+00:00"}}
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    let transport = OfflineTransport::new();

    let _result = start_agent_with_transport(&ws, &AgentId::new("alpha"), false, false, false, None, &transport);

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
    std::fs::write(team.join("TEAM.md"), "---\nname: addteam\nobjective: Add probe.\nprovider: codex\n---\n\nteam.\n").unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), QS_VALID_ROLE).unwrap(); // existing agent
    let role_file = team.join("worker2-role.md"); // OUTSIDE agents/ -> not a duplicate of an existing agent
    std::fs::write(&role_file, DELEG_ROLE_WORKER2).unwrap();
    seed_healthy_coordinator(&team);
    let transport = OfflineTransport::new();

    let _result = add_agent_with_transport(&team, &AgentId::new("worker2"), &role_file, false, None, &transport);

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
    let _ = quick_start_with_transport(&team, None, true, true, None, &transport);
    let workspace = team.parent().expect("team_workspace(<base>/teamdir) = <base>");
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
    let tasks = tasks.as_array().expect("`tasks` must be a JSON array (golden default [])");
    // (a) spec.tasks must be carried into runtime state — the doc compiler's default task id.
    assert!(
        tasks.iter().any(|t| t.get("id").and_then(|v| v.as_str()) == Some("task_initial")),
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
//   Order remains the golden prefix (spec_path … display_backend) + new suffix
//   (active_team_key, teams). display_backend stays the resolved backend from
//   display/backend.py:12-29 (default adaptive), not the raw optional spec field.
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
    let _ = quick_start_with_transport(&team, None, true, true, None, &transport);
    let workspace = team.parent().expect("team_workspace(<base>/teamdir) = <base>");
    // E5: spec_path in state now points to .team/runtime/<team_key>/, team_dir stays the user role dir.
    let spec_path = quick_start_runtime_spec_path(&team);
    let (raw, state) = raw_runtime_state(workspace);
    let keys = state.as_object().expect("state root object").keys().cloned().collect::<Vec<_>>();
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
            "active_team_key",
            "teams",
        ],
        "state.json top-level key order must match golden launch/core.py:62-71 \
         plus Bug 1/2 team-in-team suffix (active_team_key, teams). \
         Bug 2 owner team-scope (N1/N12/N18/N29) deliberately keeps owner / \
         leader_receiver / owner_epoch OFF the top level — they live ONLY under \
         teams[<active_team_key>] so per-team isolation has a single source of \
         truth (no shadow copy on the root that callers could read across teams); \
         raw={raw}"
    );
    assert_eq!(state["spec_path"], json!(spec_path.canonicalize().unwrap().to_string_lossy()));
    assert_eq!(state["workspace"], json!(workspace.to_string_lossy()));
    assert_eq!(state["team_dir"], json!(team.to_string_lossy()));
    assert_eq!(state["session_name"], json!("team-quickteam"));
    assert_eq!(
        state["leader"]["id"],
        json!("leader"),
        "leader must be copied from compiled spec.leader"
    );
    assert!(
        state["agents"].as_object().is_some_and(|agents| agents.contains_key("implementer")),
        "existing agents value must remain seeded from compiled spec; got {:?}",
        state["agents"]
    );
    assert!(
        state["tasks"].as_array().is_some_and(|tasks| {
            tasks.iter().any(|task| task.get("id").and_then(|id| id.as_str()) == Some("task_initial"))
        }),
        "existing tasks value must remain seeded from compiled spec; got {:?}",
        state["tasks"]
    );
    assert_eq!(
        state["display_backend"],
        json!("none"),
        "golden resolve_display_backend(None, source='launch') defaults to none"
    );
}

// Stage B1 — golden launch/core.py:238-255 writes the running agent state only after
// a successful spawn. The fixed clock is a test seam for `spawned_at`; capture intentionally
// misses, so session/capture fields remain present JSON nulls. MCP install is deterministic:
// provider_cli/adapter.py:111-114 writes workspace/.team/runtime/mcp/<agent>.json.
#[test]
#[serial(env)]
fn quick_start_running_agent_state_shape_after_spawn_is_golden() {
    const FIXED_SPAWNED_AT: &str = "2026-06-04T00:00:00+00:00";
    let _clock_guard =
        EnvVarGuard::set("TEAM_AGENT_TEST_FIXED_SPAWNED_AT", FIXED_SPAWNED_AT);
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let transport = OfflineTransport::new();
    let _ = quick_start_with_transport(&team, None, true, true, None, &transport);
    let workspace = team.parent().expect("team_workspace(<base>/teamdir) = <base>");
    let (raw, state) = raw_runtime_state(workspace);
    let agent = state
        .pointer("/agents/implementer")
        .unwrap_or_else(|| panic!("implementer agent state missing; raw={raw}"));
    let keys = agent.as_object().expect("agent state object").keys().cloned().collect::<Vec<_>>();
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
        "running agent state key order must match golden launch/core.py:238-255; raw={raw}"
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
        json!(workspace.join(".team/runtime/mcp/implementer.json").to_string_lossy())
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
    // D5 (#264) / Python launch/core.py:253 — fresh launch persists spawn_cwd=workspace.
    assert_eq!(agent["spawn_cwd"], json!(workspace.to_string_lossy()));
    assert_eq!(agent["spawned_at"], json!(FIXED_SPAWNED_AT));
    assert_eq!(
        agent["pane_id"],
        json!("%0"),
        "#252 RC2: launch must persist the real pane_id returned by the transport, not drop it when pane_pid is unavailable"
    );
}

// Stage B2 — golden launch/core.py:171-173 writes paused workers as exactly
// {status, provider} and skips spawn entirely.
#[test]
fn quick_start_paused_agent_state_is_paused_provider_only_and_not_spawned() {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let spec_path = compiled_spec_path_with_paused_agent(&team);
    let workspace = team.parent().expect("team_workspace(<base>/teamdir) = <base>");
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
    let keys = agent.as_object().expect("agent state object").keys().cloned().collect::<Vec<_>>();
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
