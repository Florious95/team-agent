//! Ad-hoc roles still enter through the ordinary quick-start/add-agent transactions.
use super::*;
use crate::model::yaml::Value as Yaml;
use std::io::Write;

pub(super) fn default_provider(state: Option<&Value>) -> String {
    use crate::provider::wire::{parse_provider, provider_wire};
    state
        .and_then(|state| state.pointer("/team_owner/provider"))
        .and_then(Value::as_str)
        .and_then(parse_provider)
        .or_else(|| {
            std::env::var("TEAM_AGENT_LEADER_PROVIDER")
                .ok()
                .as_deref()
                .and_then(parse_provider)
        })
        .map(provider_wire)
        .unwrap_or("pi")
        .to_owned()
}

pub(super) fn creation_patch(
    id: &str,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
    default_provider: &str,
) -> Result<RoleConfigPatch, CliError> {
    let meta = source
        .map(crate::compiler::read_front_matter)
        .transpose()
        .map_err(|e| refusal(e.to_string()))?
        .map(|(meta, _)| meta);
    let mut patch = patch.clone();
    if patch.provider.is_none() && meta.as_ref().and_then(|m| m.get("provider")).is_none() {
        patch.provider = Some(default_provider.into());
    }
    if patch.bypass.is_none()
        && meta
            .as_ref()
            .and_then(|m| m.get("dangerously_skip_permissions"))
            .is_none()
    {
        patch.bypass = Some(false);
    }
    if source.is_none() && patch.prompt.is_none() {
        patch.prompt = Some(format!(
            "You are {id}. Complete the tasks assigned by the leader.\n"
        ));
    }
    Ok(patch)
}

pub(super) fn initial_role(
    id: &str,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
    default_provider: &str,
) -> Result<String, CliError> {
    let patch = creation_patch(id, source, patch, default_provider)?;
    let text = source
        .map(std::fs::read_to_string)
        .transpose()?
        .unwrap_or_default();
    crate::lifecycle::role_config::patched_role(&text, &AgentId::new(id), &patch, source.is_none())
        .map_err(|e| refusal(e.to_string()))
}

pub(super) fn execute(
    args: &QuickStartArgs,
    agent: &str,
    role: &str,
) -> Result<CmdResult, CliError> {
    // Legacy quick-start seeds its roster before materializing provider commands.
    // Reject unavailable binaries/catalog models before that provisional seat exists.
    crate::lifecycle::seat_start::preflight_bootstrap(role).map_err(|error| refusal(error.to_string()))?;
    // The existing seat lock is reentrant for the downstream launch. Keep source
    // creation and rollback in that same exclusion domain, including concurrent starts.
    let id = AgentId::new(agent);
    let _lock = crate::lifecycle::lock::acquire_seat_lifecycle_lock(
        crate::lifecycle::lock::LifecycleLockRequest {
            workspace: &args.workspace,
            operation: "dynamic-start",
            team: args.team_id.as_deref(),
            agent_id: Some(&id),
        },
    )
    .map_err(|error| refusal(error.to_string()))?;
    execute_with(args, agent, role, cmd_quick_start)
}

fn preflight_role(role: &str) -> Result<(), CliError> {
    use crate::provider::wire::{command_name, parse_provider};
    let (meta, _) = crate::compiler::split_front_matter(role);
    let key = meta.get("provider").and_then(Yaml::as_str).unwrap_or("pi");
    // Contract providers already perform complete native admission in quick-start
    // before its initial runtime-state write. Do not route them through legacy checks.
    #[cfg(unix)]
    if crate::contract_runtime::registry::declared(key).is_some() {
        return Ok(());
    }
    let provider =
        parse_provider(key).ok_or_else(|| refusal(format!("unknown provider: {key}")))?;
    if !crate::provider::get_adapter(provider).is_installed() {
        return Err(refusal(format!(
            "Provider {key} command '{}' not found on PATH",
            command_name(provider)
        )));
    }
    if provider == crate::model::enums::Provider::Pi {
        let (_, catalog) = crate::lifecycle::launch::pi_mcp::resolve_pi_executable_chain()
            .map_err(|error| refusal(error.to_string()))?;
        if let Some(model) = meta.get("model").and_then(Yaml::as_str) {
            crate::lifecycle::launch::pi_mcp::select_exact_pi_model(&catalog, model)
                .map_err(|error| refusal(error.to_string()))?;
        }
    }
    Ok(())
}

fn execute_with(
    args: &QuickStartArgs,
    agent: &str,
    role: &str,
    launch: impl FnOnce(&QuickStartArgs) -> Result<CmdResult, CliError>,
) -> Result<CmdResult, CliError> {
    crate::lifecycle::seat_start::stage_sources(&args.workspace, &args.agents_dir,
        args.team_id.as_deref().unwrap_or("current"), agent, role, || {
            launch(args).map(|result| (result.exit == ExitCode::Ok, result))
                .map_err(|error| crate::lifecycle::LifecycleError::StatePersist(error.to_string()))
        }).map_err(|error| refusal(error.to_string()))
}

// Retained unchanged until the post-H11 implementation-excision gate.
fn execute_with_legacy_implementation(
    args: &QuickStartArgs,
    agent: &str,
    role: &str,
    launch: impl FnOnce(&QuickStartArgs) -> Result<CmdResult, CliError>,
) -> Result<CmdResult, CliError> {
    let team = args.team_id.as_deref().unwrap_or("current");
    let metadata = Yaml::Map(vec![("name".into(), Yaml::Str(team.into()))]);
    let team_text = format!("---\n{}---\n", crate::model::yaml::dumps(&metadata));
    let role_dir = args.agents_dir.join("agents");
    let directories = [&args.agents_dir, &role_dir];
    let absent: Vec<_> = directories
        .into_iter()
        .filter(|path| !path.exists())
        .collect();
    let mut created = Vec::new();
    let result = (|| {
        std::fs::create_dir_all(&role_dir)?;
        for (path, text) in [
            (args.agents_dir.join("TEAM.md"), team_text.as_str()),
            (role_dir.join(format!("{agent}.md")), role),
        ] {
            if create_source(&path, text)? {
                created.push((path, text));
            }
        }
        launch(args)
    })();
    // Keep source truth if quick-start committed a seat, even when readiness is
    // partial. Otherwise roll back only this attempt's new files, never user input.
    if !result
        .as_ref()
        .is_ok_and(|result| result.exit == ExitCode::Ok)
        && !source_adopted(args, agent)
    {
        for (path, text) in created.into_iter().rev() {
            if std::fs::read(&path).is_ok_and(|bytes| bytes == text.as_bytes()) {
                std::fs::remove_file(path)?;
            }
        }
        for path in absent.into_iter().rev() {
            match std::fs::remove_dir(path) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    result
}

fn source_adopted(args: &QuickStartArgs, agent: &str) -> bool {
    // Unlike team selection, this also accepts an empty post-rollback state.
    // Unreadable concurrent state is not permission to remove recovery inputs.
    crate::state::persist::load_runtime_state_without_migrations(&args.workspace)
        .map(|state| {
            !state.is_object()
                || crate::state::projection::project_top_level_view(
                    &state,
                    args.team_id.as_deref().unwrap_or("current"),
                )
                .get("agents")
                .and_then(|agents| agents.get(agent))
                .is_some()
        })
        .unwrap_or(true)
}

fn create_source(path: &Path, text: &str) -> Result<bool, CliError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            if let Err(error) = file
                .write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
            {
                drop(file);
                std::fs::remove_file(path)?;
                return Err(error.into());
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(path)?.is_file() && std::fs::read(path)? == text.as_bytes()
            {
                Ok(false)
            } else {
                Err(refusal(format!(
                    "ad-hoc source changed: {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::super::tests::Sandbox;
    use super::*;

    fn parse_start(root: &Path, tokens: &[&str]) -> Result<Intent, CliError> {
        let tokens: Vec<String> = tokens.iter().map(|s| s.to_string()).collect();
        let checked = crate::cli::arguments::parse("intent-start", &tokens)?;
        super::super::lifecycle_intent("start", Verb::Start, &checked.tokens, root, false, false)
    }

    fn resolved_start(step: &Step) -> (QuickStartArgs, String, String) {
        let Step::UnifiedStart { request, .. } = step else { panic!("expected unified start") };
        let selected = crate::state::persist::runtime_state_path(&request.workspace).is_file().then(||
            crate::state::selector::resolve_active_team_readonly(&request.workspace, request.team.as_deref(),
                crate::state::selector::SelectorMode::RuntimeOnly).unwrap());
        let declaration = crate::lifecycle::seat_start::resolve(request, selected.as_ref()).unwrap();
        let args = QuickStartArgs { workspace: request.workspace.clone(),
            agents_dir: crate::lifecycle::seat_start::bootstrap_directory(request), name: None,
            team_id: request.team.clone(), yes: false, json: true, detail: true, backend: None };
        (args, request.agent.to_string(), declaration.text)
    }

    #[test]
    #[serial_test::serial(env)]
    fn empty_workspace_admission_is_current_and_planning_is_readonly() {
        let root = Sandbox::new();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        let (args, agent, role) = resolved_start(&plan.steps[0]);
        assert_eq!(agent, "worker-1");
        assert_eq!(args.team_id.as_deref(), Some("current"));
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(
            meta.get("provider").and_then(Yaml::as_str),
            Some(default_provider(None).as_str())
        );
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
        runtime::recheck(&plan, &plan.steps[0], false).unwrap();
        assert!(meta.get("model").is_none());
        assert!(body.contains("worker-1"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    #[serial_test::serial(env)]
    fn leader_only_bootstrap_creates_first_worker_without_role_sources() {
        let root = Sandbox::new();
        let state_path = crate::state::persist::runtime_state_path(root.path());
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let state = json!({"active_team_key":"current", "team_key":"current", "teams":{"current":{
            "agents":{}, "session_name":null, "tasks":{},
            "team_owner":{"pane_id":"%0", "provider":"claude", "owner_epoch":1},
            "leader_receiver":{"pane_id":"%0", "status":"attached"}
        }}});
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(&state_path, &bytes).unwrap();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        let (_, agent, role) = resolved_start(&plan.steps[0]);
        assert_eq!(agent, "worker-1");
        let (meta, _) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("claude"));
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        assert_eq!(std::fs::read(&state_path).unwrap(), bytes);
        assert!(!root.path().join(".team/start-source").exists());
    }

    #[test]
    fn trailing_prompt_and_options_customize_the_generated_role() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &[
                "worker-1",
                "--provider",
                "claude",
                "--model",
                "sonnet",
                "--bypass",
                "true",
                "Review",
                "the",
                "changes",
            ],
        )
        .unwrap();
        assert_eq!(intent.patch.prompt.as_deref(), Some("Review the changes"));
        let role = initial_role("worker-1", None, &intent.patch, "pi").unwrap();
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("claude"));
        assert_eq!(meta.get("model").and_then(Yaml::as_str), Some("sonnet"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(true))
        );
        assert_eq!(body.trim(), "Review the changes");
    }

    #[test]
    fn literal_trailing_options_remain_prompt_text() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &["worker-1", "--", "--provider=bogus", "--help"],
        )
        .unwrap();
        assert_eq!(intent.patch.provider, None);
        assert_eq!(
            intent.patch.prompt.as_deref(),
            Some("--provider=bogus --help")
        );
        assert!(parse_start(root.path(), &["worker-1", "text", "--prompt", "other"]).is_err());
    }

    #[test]
    fn existing_role_policy_and_source_are_preserved() {
        let root = Sandbox::new();
        let path = root.path().join("worker-1.md");
        let text = "---\nname: worker-1\nprovider: claude\nmodel: sonnet\ndangerously_skip_permissions: true\n---\nOriginal task\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            initial_role("worker-1", Some(&path), &RoleConfigPatch::default(), "pi").unwrap(),
            text
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn supplied_role_without_policy_gets_safe_defaults() {
        let root = Sandbox::new();
        let path = root.path().join("worker-1.md");
        std::fs::write(&path, "Review the changes\n").unwrap();
        let role =
            initial_role("worker-1", Some(&path), &RoleConfigPatch::default(), "pi").unwrap();
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("pi"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
        assert_eq!(body.trim(), "Review the changes");
    }

    #[test]
    #[serial_test::serial(env)]
    fn refused_initial_start_removes_sources_and_allows_changed_retry() {
        let root = Sandbox::new();
        let mut plan = runtime::plan(parse_start(root.path(), &["worker-1"]).unwrap()).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        assert!(execute_with(&args, &agent, &role, |_| Err(refusal("preflight failed"))).is_err());
        assert!(!args.agents_dir.exists());
        assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
        let changed = initial_role(&agent, None, &RoleConfigPatch::default(), "claude").unwrap();
        execute_with(&args, &agent, &changed, |_| {
            Ok(CmdResult::from_json(json!({"ok":true}), true))
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(args.agents_dir.join("agents/worker-1.md")).unwrap(),
            changed
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn failed_provider_admission_never_seeds_a_seat_or_role_source() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &[
                "worker-1",
                "--provider",
                "pi",
                "--model",
                "team-agent-tests/missing-model",
            ],
        )
        .unwrap();
        let mut plan = runtime::plan(intent).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        assert!(execute(&args, &agent, &role).is_err());
        assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
        assert!(!args.agents_dir.exists());
    }

    #[test]
    #[serial_test::serial(env)]
    fn partial_start_keeps_sources_adopted_by_runtime() {
        let root = Sandbox::new();
        let mut plan = runtime::plan(parse_start(root.path(), &["worker-1"]).unwrap()).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        let result = execute_with(&args, &agent, &role, |_| {
            let state = json!({"active_team_key":"current", "team_key":"current", "teams":{"current":{
                "team_key":"current", "status":"alive", "agents":{"worker-1":{"status":"starting"}}
            }}});
            let state_path = crate::state::persist::runtime_state_path(root.path());
            std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
            std::fs::write(state_path, serde_json::to_vec(&state).unwrap()).unwrap();
            Ok(CmdResult::from_json(json!({"ok":false,"status":"partial"}), true))
        }).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        assert!(source_adopted(&args, &agent));
        assert_eq!(
            std::fs::read_to_string(args.agents_dir.join("agents/worker-1.md")).unwrap(),
            role
        );
    }

    #[test]
    fn managed_sources_never_overwrite_changed_documents() {
        let root = Sandbox::new();
        let path = root.path().join("role.md");
        create_source(&path, "original").unwrap();
        create_source(&path, "original").unwrap();
        assert!(create_source(&path, "changed").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
}
