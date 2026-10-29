//! WorkingDirectory grants remain captured owner/descriptor grants, not an
//! arbitrary second filesystem root. Real no-follow/CAS checks run on Unix.
#![cfg(unix)]
mod support;
use std::path::PathBuf;
use support::*;
use team_agent_contract::contract::{descriptor::*, hooks::*, plan::*, types::*};
use team_agent_contract::host::{command::*, materialize::*, process::resolve_cwd};
use team_agent_contract::orchestration::{lifecycle::*, store::*};

struct CwdPlan;
impl PlanHook for CwdPlan {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let mut plan = Fake.plan(resolved)?;
        if resolved.request().mode == LaunchMode::LaunchOnly {
            plan.arguments.truncate(2); // no FullWorker MCP arguments in this mode
            plan.carriers.mcp = CarrierUse::NotRequested;
        }
        plan.materialization[0].path = OwnedPath::new(
            resolved.request().paths.cwd.path.clone(),
            PathBuf::from(".kiro/agents/owned.json"),
        )?;
        Ok(plan)
    }
}
struct NoCommands;
impl CommandRunner for NoCommands {
    fn run(&mut self, _: &CommandRequest) -> CommandReceipt {
        panic!("no command authorized by filesystem test")
    }
}
fn fixture() -> (
    Sandbox,
    ProviderDescriptor,
    LaunchRequest,
    ResolvedLaunch,
    LaunchPlan,
) {
    let sandbox = Sandbox::new();
    std::fs::create_dir(&sandbox.root).unwrap();
    let cwd = sandbox.parent.join("workspace");
    std::fs::create_dir(&cwd).unwrap();
    let mut d = descriptor();
    d.workspace.config_scope = ResourceScope::WorkingDirectory;
    d.workspace.shared_cwd = true;
    d.workspace.requires_materialization_lease = true;
    let mut request = request(&sandbox.root, "worker");
    request.paths.cwd = resolve_cwd(&cwd).unwrap();
    let mut hooks = hooks();
    hooks.plan = HookBinding::Bound(&CwdPlan);
    let resolved = resolve_launch(&d, &hooks, &request, None).unwrap();
    let plan = CwdPlan.plan(&resolved).unwrap();
    (sandbox, d, request, resolved, plan)
}

#[test]
fn lifecycle_captures_descriptor_workspace_classes_and_teardown_uses_the_same_grant() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut req = request(&sandbox.root, "worker");
    let cwd = sandbox.parent.join("code");
    std::fs::create_dir(&cwd).unwrap();
    req.paths.cwd = resolve_cwd(&cwd).unwrap();
    let mut d = descriptor();
    d.workspace.config_scope = ResourceScope::WorkingDirectory;
    let mut h = hooks();
    h.plan = HookBinding::Bound(&CwdPlan);
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("cwd-start");
    let operation = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &req,
        routing("worker"),
        OperationId::new("cwd-start").unwrap(),
    )
    .unwrap();
    assert_eq!(operation.outcome, Outcome::Committed);
    assert_eq!(
        operation.target.workspace_resources,
        vec![ResourceKind::Prompt]
    );
    assert_eq!(operation.resources[0].path.root(), cwd);
    assert_ne!(cwd, sandbox.root);
    let stop = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(
        &operation.target.identity,
        OperationId::new("cwd-stop").unwrap(),
    )
    .unwrap();
    assert_eq!(stop.outcome, Outcome::Committed);
    assert_eq!(host.removed, 1);
    assert!(stop.preserved.is_empty());
}

#[test]
fn captured_validator_uses_required_path_option_without_shell_splitting() {
    use std::time::Duration;
    struct Validator {
        executable: PathBuf,
        cwd: PathBuf,
        path: PathBuf,
        calls: usize,
    }
    impl CommandRunner for Validator {
        fn run(&mut self, command: &CommandRequest) -> CommandReceipt {
            self.calls += 1;
            assert_eq!(command.executable, self.executable);
            assert_eq!(command.cwd.as_ref(), Some(&self.cwd));
            assert_eq!(
                command.arguments,
                vec![
                    std::ffi::OsString::from("agent"),
                    "validate".into(),
                    "--path".into(),
                    self.path.clone().into_os_string(),
                ]
            );
            assert!(command.stdin.is_none());
            CommandReceipt {
                end: CommandEnd::Exited,
                exit_code: Some(0),
                child_pid: Some(42),
                child_reaped: true,
                stdout: vec![],
                stderr: vec![],
                elapsed: Duration::from_millis(1),
            }
        }
    }
    let (sandbox, d, mut request, _, _) = fixture();
    let cwd = sandbox.parent.join("workspace with 'quote' 中文");
    std::fs::create_dir(&cwd).unwrap();
    request.paths.cwd = resolve_cwd(&cwd).unwrap();
    // Validation runs the same public command the pane would exec.
    let bin = sandbox.parent.join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(bin.join("fake"), b"controlled validator; never executed").unwrap();
    request.paths.launcher = LaunchEntrypoint {
        command: "fake".into(),
        search_path: bin.clone().into_os_string(),
        executable: bin.join("fake"),
        sha256: team_agent_contract::host::digest(b"controlled validator; never executed"),
    };
    // This exercises owned configuration validation, not the fake terminal's
    // FullWorker profile (which is deliberately pinned to its own image hash).
    request.mode = LaunchMode::LaunchOnly;
    let mut h = hooks();
    h.plan = HookBinding::Bound(&CwdPlan);
    let resolved = resolve_launch(&d, &h, &request, None).unwrap();
    let plan = CwdPlan.plan(&resolved).unwrap();
    validate_launch_plan(&d, &resolved, &plan).unwrap();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("validate").unwrap(),
        Validator {
            executable: request.paths.launcher.executable,
            cwd,
            path: plan.materialization[0].path.path(),
            calls: 0,
        },
    )
    .unwrap();
    let receipt = io.create_exclusive(&plan.materialization[0]).unwrap();
    let output = io
        .validate_configuration(&OwnedValidationRequest {
            resource: receipt,
            validator_id: "kiro-agent-validate",
            bounds: ReadBounds {
                deadline: Duration::from_secs(2),
                max_output_bytes: 4096,
            },
        })
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(io.runner().calls, 1);
}

#[test]
fn runtime_only_descriptor_cannot_write_a_working_directory_request() {
    let (_sandbox, mut d, _request, resolved, plan) = fixture();
    d.workspace.config_scope = ResourceScope::RuntimeRoot;
    assert!(ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands
    )
    .is_err());
    assert!(!plan.materialization[0].path.path().exists());
}

#[test]
fn valid_workspace_write_is_exclusive_and_records_physical_identity() {
    let (_sandbox, d, request, resolved, plan) = fixture();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands,
    )
    .unwrap();
    let receipt = io.create_exclusive(&plan.materialization[0]).unwrap();
    assert!(receipt.creation_identity.is_some());
    assert!(receipt.exclusive);
    let before = std::fs::read(receipt.path.path()).unwrap();
    assert_eq!(before, plan.materialization[0].contents);
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());
    assert_eq!(std::fs::read(receipt.path.path()).unwrap(), before);
    assert!(remove_quiescent(&request.paths.cwd, &request.identity, &receipt).unwrap());
    assert!(!receipt.path.path().exists());
}

#[test]
fn unplanned_path_owner_and_contents_are_rejected_before_filesystem_effects() {
    let (_sandbox, d, _request, resolved, plan) = fixture();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands,
    )
    .unwrap();
    for change in 0..3 {
        let mut request = plan.materialization[0].clone();
        match change {
            0 => request.owner.instance = InstanceId::new("foreign").unwrap(),
            1 => request.contents.push(42),
            _ => {
                request.path =
                    OwnedPath::new(request.path.root().into(), "other.json".into()).unwrap()
            }
        }
        assert!(io.create_exclusive(&request).is_err());
        assert!(!request.path.path().exists());
    }
}

#[test]
fn symlink_config_directory_cannot_escape_the_captured_workspace() {
    use std::os::unix::fs::symlink;
    let (sandbox, d, request, resolved, plan) = fixture();
    let outside = sandbox.parent.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, request.paths.cwd.path.join(".kiro")).unwrap();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands,
    )
    .unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());
    assert!(!outside.join("agents/owned.json").exists());
}

#[test]
fn replacing_workspace_after_grant_capture_cannot_transfer_authority() {
    let (sandbox, d, request, resolved, plan) = fixture();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands,
    )
    .unwrap();
    std::fs::rename(
        &request.paths.cwd.path,
        sandbox.parent.join("old-workspace"),
    )
    .unwrap();
    std::fs::create_dir(&request.paths.cwd.path).unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());
    assert!(!plan.materialization[0].path.path().exists());
}

#[test]
fn dirty_replaced_and_foreign_owned_files_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    for mode in 0..3 {
        let (_sandbox, d, request, resolved, plan) = fixture();
        let mut io = ScopedMaterializer::new(
            &d,
            &resolved,
            &plan,
            OperationId::new("op").unwrap(),
            NoCommands,
        )
        .unwrap();
        let receipt = io.create_exclusive(&plan.materialization[0]).unwrap();
        let mut owner = request.identity.clone();
        match mode {
            0 => std::fs::write(receipt.path.path(), b"user changes").unwrap(),
            1 => {
                let replacement = receipt.path.path().with_extension("replacement");
                std::fs::write(&replacement, &plan.materialization[0].contents).unwrap();
                std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                std::fs::rename(replacement, receipt.path.path()).unwrap();
            }
            _ => owner.instance = InstanceId::new("other").unwrap(),
        }
        assert!(!remove_quiescent(&request.paths.cwd, &owner, &receipt).unwrap());
        assert!(receipt.path.path().exists());
    }
}

#[test]
fn shared_writable_directory_is_not_silently_chmoded_or_adopted() {
    use std::os::unix::fs::PermissionsExt;
    let (_sandbox, d, request, resolved, plan) = fixture();
    let original = std::fs::metadata(&request.paths.cwd.path)
        .unwrap()
        .permissions();
    std::fs::set_permissions(
        &request.paths.cwd.path,
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    let mut io = ScopedMaterializer::new(
        &d,
        &resolved,
        &plan,
        OperationId::new("op").unwrap(),
        NoCommands,
    )
    .unwrap();
    assert!(io.create_exclusive(&plan.materialization[0]).is_err());
    assert_eq!(
        std::fs::metadata(&request.paths.cwd.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o777
    );
    std::fs::set_permissions(&request.paths.cwd.path, original).unwrap();
}

/// One seat's grant to merge its entry into `<project>/<file>`.
fn shared_entry_seat(
    sandbox: &Sandbox,
    project: &std::path::Path,
    seat: &str,
    file: &str,
    contents: impl Fn(&str) -> Vec<u8>,
) -> (ScopedMaterializer<NoCommands>, OwnedResourceRequest) {
    let mut d = descriptor();
    d.mcp.scope = ResourceScope::WorkingDirectory;
    d.workspace.shared_cwd = true;
    d.teardown.resources = &[
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::SharedEntry },
    ];
    let mut request = request(&sandbox.root, seat);
    request.paths.cwd = resolve_cwd(project).unwrap();
    let resolved = resolve_launch(&d, &hooks(), &request, None).unwrap();
    let mut plan = Fake.plan(&resolved).unwrap();
    let entry = OwnedResourceRequest {
        path: OwnedPath::new(project.into(), file.into()).unwrap(),
        owner: request.identity.clone(),
        kind: ResourceKind::McpConfig,
        contents: contents(&shared_entry_key(&request.identity)),
    };
    plan.materialization.push(entry.clone());
    let io = ScopedMaterializer::new(&d, &resolved, &plan, OperationId::new(format!("start-{seat}")).unwrap(), NoCommands).unwrap();
    (io, entry)
}

fn json_entry(key: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"mcpServers": {key: {"command": "/candidate", "args": [key], "env": {}}}})).unwrap()
}

#[test]
fn seat_entries_join_and_leave_a_shared_json_file_without_touching_the_users() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    std::fs::create_dir(&sandbox.root).unwrap();
    let project = sandbox.parent.join("project");
    std::fs::create_dir_all(project.join(".cursor")).unwrap();
    let file = project.join(".cursor/mcp.json");
    std::fs::write(&file, br#"{"mcpServers":{"user":{"command":"mine"}},"other":1}"#).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let root = resolve_cwd(&project).unwrap();
    let (mut a, entry_a) = shared_entry_seat(&sandbox, &project, "alpha", ".cursor/mcp.json", json_entry);
    let (mut b, entry_b) = shared_entry_seat(&sandbox, &project, "beta", ".cursor/mcp.json", json_entry);
    let receipt_a = a.merge_shared_entry(&entry_a).unwrap();
    let receipt_b = b.merge_shared_entry(&entry_b).unwrap();
    assert_eq!(receipt_a.disposition, ResourceDisposition::SharedEntry);
    assert!(!receipt_a.exclusive);
    team_agent_contract::host::materialize::verify_shared_entry(&entry_a).unwrap();
    team_agent_contract::host::materialize::verify_shared_entry(&entry_b).unwrap();
    // A seat is granted only its own planned entry.
    assert!(a.merge_shared_entry(&entry_b).is_err());
    assert!(a.create_exclusive(&entry_a).is_err());
    let key_a = shared_entry_key(&entry_a.owner);
    let key_b = shared_entry_key(&entry_b.owner);
    let read = || serde_json::from_slice::<serde_json::Value>(&std::fs::read(&file).unwrap()).unwrap();
    let value = read();
    assert_eq!(value["other"], 1);
    assert_eq!(value["mcpServers"]["user"]["command"], "mine");
    assert_eq!(value["mcpServers"][&key_a]["args"][0], key_a.as_str());
    assert_eq!(value["mcpServers"][&key_b]["args"][0], key_b.as_str());
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
    assert!(remove_quiescent(&root, &entry_a.owner, &receipt_a).unwrap());
    assert!(remove_quiescent(&root, &entry_a.owner, &receipt_a).unwrap(), "already gone is removed");
    // Another owner's receipt never removes anything.
    assert!(!remove_quiescent(&root, &entry_a.owner, &receipt_b).unwrap());
    let value = read();
    assert!(value["mcpServers"].get(&key_a).is_none());
    assert!(team_agent_contract::host::materialize::verify_shared_entry(&entry_a).is_err());
    team_agent_contract::host::materialize::verify_shared_entry(&entry_b).unwrap();
    assert_eq!(value["mcpServers"][&key_b]["command"], "/candidate");
    assert!(remove_quiescent(&root, &entry_b.owner, &receipt_b).unwrap());
    assert_eq!(read(), serde_json::json!({"mcpServers": {"user": {"command": "mine"}}, "other": 1}));
    // No temporary file is left behind.
    assert_eq!(std::fs::read_dir(project.join(".cursor")).unwrap().count(), 1);
}

#[test]
fn a_file_created_for_a_seat_entry_is_removed_with_it_and_invalid_user_files_are_kept() {
    let sandbox = Sandbox::new();
    std::fs::create_dir(&sandbox.root).unwrap();
    let project = sandbox.parent.join("project");
    std::fs::create_dir(&project).unwrap();
    let root = resolve_cwd(&project).unwrap();
    let file = project.join(".cursor/mcp.json");
    let (mut seat, entry) = shared_entry_seat(&sandbox, &project, "alpha", ".cursor/mcp.json", json_entry);
    let receipt = seat.merge_shared_entry(&entry).unwrap();
    assert!(file.is_file());
    assert!(remove_quiescent(&root, &entry.owner, &receipt).unwrap());
    assert!(!file.exists());
    // A user's file that is not valid JSON is never rewritten.
    std::fs::write(&file, b"{ not json").unwrap();
    let (mut seat, entry) = shared_entry_seat(&sandbox, &project, "beta", ".cursor/mcp.json", json_entry);
    let failure = seat.merge_shared_entry(&entry).unwrap_err();
    assert!(matches!(failure.error, ContractError::Invalid(message) if message.contains("not valid JSON")));
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not json");
    // A symlinked configuration file is refused, not followed.
    std::fs::remove_file(&file).unwrap();
    std::fs::write(project.join("elsewhere.json"), b"{}").unwrap();
    std::os::unix::fs::symlink(project.join("elsewhere.json"), &file).unwrap();
    assert!(seat.merge_shared_entry(&entry).is_err());
    assert_eq!(std::fs::read(project.join("elsewhere.json")).unwrap(), b"{}");
}

#[test]
fn seat_entries_join_and_leave_a_shared_toml_file_byte_for_byte() {
    let sandbox = Sandbox::new();
    std::fs::create_dir(&sandbox.root).unwrap();
    let project = sandbox.parent.join("project");
    std::fs::create_dir_all(project.join(".grok")).unwrap();
    let file = project.join(".grok/config.toml");
    let user = "# user settings\n[mcp_servers.user]\ncommand = \"mine\"\nargs = [\"a\"]";
    std::fs::write(&file, user).unwrap();
    let root = resolve_cwd(&project).unwrap();
    let toml = |key: &str| format!("[mcp_servers.{key}]\ncommand = \"/candidate\"\n").into_bytes();
    let (mut a, entry_a) = shared_entry_seat(&sandbox, &project, "alpha", ".grok/config.toml", toml);
    let (mut b, entry_b) = shared_entry_seat(&sandbox, &project, "beta", ".grok/config.toml", toml);
    let receipt_a = a.merge_shared_entry(&entry_a).unwrap();
    let receipt_b = b.merge_shared_entry(&entry_b).unwrap();
    // Merging the same entry again does not duplicate it.
    a.merge_shared_entry(&entry_a).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    team_agent_contract::host::materialize::verify_shared_entry(&entry_a).unwrap();
    team_agent_contract::host::materialize::verify_shared_entry(&entry_b).unwrap();
    let mut tampered = entry_a.clone();
    tampered.contents = b"changed\n".to_vec();
    assert!(team_agent_contract::host::materialize::verify_shared_entry(&tampered).is_err());
    assert!(text.starts_with(&format!("{user}\n")));
    let key_a = shared_entry_key(&entry_a.owner);
    assert_eq!(text.matches(&format!("[mcp_servers.{key_a}]")).count(), 1);
    assert!(text.contains(&format!("[mcp_servers.{}]", shared_entry_key(&entry_b.owner))));
    assert!(remove_quiescent(&root, &entry_a.owner, &receipt_a).unwrap());
    assert!(remove_quiescent(&root, &entry_b.owner, &receipt_b).unwrap());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{user}\n"));
}

struct SharedEntryPlan;
impl PlanHook for SharedEntryPlan {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let mut plan = Fake.plan(resolved)?;
        plan.materialization.push(OwnedResourceRequest {
            path: OwnedPath::new(r.paths.cwd.path.clone(), ".cursor/mcp.json".into())?,
            owner: r.identity.clone(),
            kind: ResourceKind::McpConfig,
            contents: json_entry(&shared_entry_key(&r.identity)),
        });
        Ok(plan)
    }
}
impl MaterializeHook for SharedEntryPlan {
    fn materialize(&self, requests: &[OwnedResourceRequest], io: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        let mut resources = vec![];
        for request in requests {
            resources.push(if request.kind == ResourceKind::McpConfig {
                io.merge_shared_entry(request)?
            } else {
                io.create_exclusive(request)?
            });
        }
        Ok(MaterializeReceipt { resources })
    }
}

#[test]
fn lifecycle_journals_a_shared_entry_in_the_project_and_teardown_removes_it() {
    let sandbox = Sandbox::new();
    let project = sandbox.parent.join("project");
    std::fs::create_dir(&project).unwrap();
    let mut store = sandbox.store();
    let mut d = descriptor();
    d.mcp.scope = ResourceScope::WorkingDirectory;
    d.workspace.shared_cwd = true;
    d.teardown.resources = &[
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::SharedEntry },
    ];
    let mut h = hooks();
    h.plan = HookBinding::Bound(&SharedEntryPlan);
    h.materialize = HookBinding::Bound(&SharedEntryPlan);
    let mut req = request(&sandbox.root, "worker");
    req.paths.cwd = resolve_cwd(&project).unwrap();
    let resolved = resolve_launch(&d, &h, &req, None).unwrap();
    let plan = SharedEntryPlan.plan(&resolved).unwrap();
    let mut io = ScopedMaterializer::new(&d, &resolved, &plan, OperationId::new("shared-start").unwrap(), NoCommands).unwrap();
    let mut host = FakeHost::default();
    let operation = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
        .startup(&Adapter { descriptor: &d, hooks: &h, catalog: None }, &req, routing("worker"),
            OperationId::new("shared-start").unwrap())
        .unwrap();
    assert_eq!(operation.outcome, Outcome::Committed, "{:?}", operation.failure);
    assert_eq!(operation.target.workspace_resources, vec![ResourceKind::McpConfig]);
    let entry = operation.resources.iter().find(|r| r.kind == ResourceKind::McpConfig).unwrap();
    assert_eq!(entry.disposition, ResourceDisposition::SharedEntry);
    assert_eq!(entry.path.root(), project);
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(project.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert!(value["mcpServers"].get(shared_entry_key(&req.identity)).is_some());
    let stop = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
        .teardown(&operation.target.identity, OperationId::new("shared-stop").unwrap())
        .unwrap();
    assert_eq!(stop.outcome, Outcome::Committed);
    assert_eq!(host.removed, 2, "prompt file and project entry");
    assert!(stop.preserved.is_empty());
}
