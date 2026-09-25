//! Independent R01–R12 contracts for Pi `fork-agent` session inheritance.
//!
//! These tests exercise the public lifecycle port and existing lifecycle/state
//! surfaces only. No implementation-only fork helper is assumed by the baseline.
//! The frozen baseline is expected to fail R01 at its current Pi native-fork
//! refusal, after the fixture has independently proved a valid source tuple.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate as team_agent;
use serde_json::{Value, json};
use serial_test::serial;
use team_agent::lifecycle::launch::{fork_agent_with_transport, pi_mcp::pi_seat_paths};
use team_agent::model::ids::AgentId;
use team_agent::provider::{AuthMode, Provider, SessionId, get_adapter};
use team_agent::state::persist::{load_runtime_state, runtime_state_path};
use team_agent::transport::test_support::OfflineTransport;

const SOURCE: &str = "implementer";
const TARGET: &str = "variant";
const SESSION_A: &str = "652756ea-aaee-4437-a30f-40c5606826d5";
const CAPTURED_AT: &str = "2026-09-12T10:00:02Z";
const SESSION_TIMESTAMP: &str = "2025-01-02T03:04:05.000Z";
const PROPERTY_SEED: u64 = 0x5eed_f0a7_1209_2026;

const ROLE_DOC: &str = "---\nname: implementer\nrole: Session inheritance fixture\nprovider: pi\nmodel: team-agent/qwen3.8-27b\nauth_mode: subscription\neffort: high\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\ncommunication_mode: orchestrated\n---\n\nKeep the complete role body: 雪だるま 🛰️.\n";
const SOURCE_ROLE: &str = "---\nname: implementer\nrole: Dynamic source role\nprovider: pi\nmodel: team-agent/qwen3.8-27b\nauth_mode: subscription\neffort: high\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\ncommunication_mode: orchestrated\nprofile: research\n---\n\nDynamic body stays exact: use Δ, 雪, and the captured tool result.\n";

#[cfg(unix)]
struct PathEnvGuard {
    previous: Option<std::ffi::OsString>,
}

#[cfg(unix)]
impl PathEnvGuard {
    fn with_fake_pi(root: &Path) -> Self {
        let wrapper_bin = root.join("bin/wrapper");
        let real_bin = root.join("bin/real");
        let package_root = root.join("node_modules/pi-mcp-adapter");
        fs::create_dir_all(&wrapper_bin).expect("create fake wrapper bin");
        fs::create_dir_all(&real_bin).expect("create fake real bin");
        fs::create_dir_all(&package_root).expect("create fake adapter package");
        let wrapper = wrapper_bin.join("pi");
        let wrapper_script = format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 0.87.1 ;;\n--list-models) printf 'provider model\\nteam-agent qwen3.8-27b\\n' ;;\nlist) printf 'npm:pi-mcp-adapter\\n{}\\n' ;;\nesac\nexit 0\n",
            package_root.display()
        );
        fs::write(&wrapper, wrapper_script).expect("write offline Pi wrapper");
        let real = real_bin.join("pi");
        fs::write(&real, "#!/bin/sh\necho 0.87.1\nexit 0\n")
            .expect("write offline Pi real-binary stand-in");
        let package =
            json!({"name":"pi-mcp-adapter","version":"0.1.0","pi":{"extensions":["./index.ts"]}});
        fs::write(package_root.join("package.json"), package.to_string())
            .expect("write offline adapter package metadata");
        fs::write(package_root.join("index.ts"), "export {};\n")
            .expect("write offline adapter extension entry");
        for pi in [&wrapper, &real] {
            let mut permissions = fs::metadata(pi).expect("fake Pi metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(pi, permissions).expect("make fake Pi executable");
        }
        let previous = std::env::var_os("PATH");
        let mut paths = vec![wrapper_bin, real_bin];
        if let Some(existing) = previous.as_deref() {
            paths.extend(std::env::split_paths(existing));
        }
        let path = std::env::join_paths(paths).expect("join fixture PATH");
        unsafe { std::env::set_var("PATH", path) };
        let version = std::process::Command::new("pi")
            .arg("--version")
            .output()
            .expect("fake Pi must resolve through fixture PATH");
        assert!(version.status.success(), "fake Pi must be executable");
        Self { previous }
    }
}

#[cfg(unix)]
impl Drop for PathEnvGuard {
    fn drop(&mut self) {
        if let Some(path) = self.previous.take() {
            unsafe { std::env::set_var("PATH", path) };
        } else {
            unsafe { std::env::remove_var("PATH") };
        }
    }
}

struct Fixture {
    #[cfg(unix)]
    _path_guard: PathEnvGuard,
    root: PathBuf,
    team_dir: PathBuf,
    run_workspace: PathBuf,
    team_key: String,
    state_path: PathBuf,
    source_paths: team_agent::lifecycle::launch::pi_mcp::PiSeatPaths,
    source_file: PathBuf,
    source_body: Vec<u8>,
    dynamic_role: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = super::temp_ws();
        #[cfg(unix)]
        let path_guard = PathEnvGuard::with_fake_pi(&root);
        let team_dir = root.join("teamdir");
        fs::create_dir_all(team_dir.join("agents")).expect("create team roles");
        fs::create_dir_all(team_dir.join("profiles")).expect("create role profiles");
        fs::write(
            team_dir.join("TEAM.md"),
            "---\nname: fork-contract-team\nobjective: Session inheritance fixture.\nprovider: codex\n---\n\nTeam fixture.\n",
        )
        .expect("write team definition");
        fs::write(team_dir.join("agents").join("implementer.md"), ROLE_DOC)
            .expect("write source role");
        fs::write(
            team_dir.join("profiles").join("research.yaml"),
            "temperature: 0.25\n",
        )
        .expect("write referenced profile");

        let setup = team_agent::lifecycle::launch::quick_start_with_transport(
            &team_dir,
            None,
            true,
            None,
            &OfflineTransport::new(),
        )
        .expect("valid Pi team fixture must quick-start");
        assert!(
            matches!(setup, team_agent::lifecycle::QuickStartReport::Ready { .. }),
            "fixture must be a started/selectable team, not a failed quick-start: {setup:?}"
        );
        let selected = team_agent::state::selector::resolve_active_team(
            &team_dir,
            None,
            team_agent::state::selector::SelectorMode::RequireSpec,
        )
        .expect("quick-start must create a selectable team with a spec");
        let run_workspace = selected.run_workspace.clone();
        let team_key = selected.team_key.clone();
        let state_path = runtime_state_path(&run_workspace);
        let source_paths = pi_seat_paths(&run_workspace, &team_key, SOURCE);
        let source_file = source_paths.sessions.join("2025/01/02/session-A.jsonl");
        fs::create_dir_all(source_file.parent().expect("source file parent"))
            .expect("create source session tree");
        let source_body = valid_v3_body();
        write_session(
            &source_file,
            SESSION_A,
            &run_workspace,
            SESSION_TIMESTAMP,
            &source_body,
        );

        let dynamic_role = run_workspace
            .join(".team/runtime/dynamic-role-files")
            .join(&team_key)
            .join("implementer.md");
        fs::create_dir_all(dynamic_role.parent().expect("dynamic role parent"))
            .expect("create dynamic role directory");
        fs::write(&dynamic_role, SOURCE_ROLE).expect("write current dynamic role");

        let mut state = selected.state.clone();
        if !state.get("agents").is_some_and(Value::is_object) {
            state["agents"] = json!({});
        }
        if !state["agents"][SOURCE].is_object() {
            state["agents"][SOURCE] = json!({});
        }
        let source = state["agents"][SOURCE]
            .as_object_mut()
            .expect("source row is an object");
        source.insert("provider".to_string(), json!("pi"));
        source.insert("auth_mode".to_string(), json!("subscription"));
        source.insert(
            "spawn_cwd".to_string(),
            json!(run_workspace.to_string_lossy().to_string()),
        );
        source.insert("session_id".to_string(), json!(SESSION_A));
        source.insert(
            "rollout_path".to_string(),
            json!(source_file.to_string_lossy().to_string()),
        );
        source.insert("captured_at".to_string(), json!(CAPTURED_AT));
        source.insert("captured_via".to_string(), json!("session_scan"));
        source.insert("capture_state".to_string(), json!("captured"));
        source.insert(
            "dynamic_role_file".to_string(),
            json!(dynamic_role.to_string_lossy().to_string()),
        );
        team_agent::state::projection::save_team_scoped_state(&run_workspace, &state)
            .expect("persist valid Pi source tuple");

        let fixture = Self {
            #[cfg(unix)]
            _path_guard: path_guard,
            root,
            team_dir,
            run_workspace,
            team_key,
            state_path,
            source_paths,
            source_file,
            source_body,
            dynamic_role,
        };
        fixture.assert_valid_source_fixture();
        fixture
    }

    fn assert_valid_source_fixture(&self) {
        let state = self.state();
        let source = &state["agents"][SOURCE];
        assert_eq!(source["provider"], "pi", "fixture source must be Pi");
        assert_eq!(source["auth_mode"], "subscription");
        assert_eq!(source["session_id"], SESSION_A);
        assert_eq!(
            source["rollout_path"].as_str(),
            Some(self.source_file.to_string_lossy().as_ref())
        );
        assert!(
            self.source_file.is_file(),
            "source backing must be a regular file"
        );
        let header = read_header(&self.source_file);
        assert_eq!(header["version"], 3, "source fixture must be a v3 session");
        assert_eq!(header["id"], SESSION_A);
        assert_eq!(header["cwd"], self.run_workspace.to_string_lossy().as_ref());
        assert_eq!(header["timestamp"], SESSION_TIMESTAMP);
        let spawned_at = source["spawned_at"]
            .as_str()
            .expect("source spawn generation");
        assert!(header["timestamp"].as_str().unwrap() < spawned_at);
        assert_valid_entry_tree(&body_bytes(&self.source_file));
        assert!(!self.dynamic_role.as_os_str().is_empty());
    }

    fn state(&self) -> Value {
        let full = load_runtime_state(&self.run_workspace).expect("load fixture runtime state");
        team_agent::state::projection::project_top_level_view(&full, &self.team_key)
    }

    fn save_state(&self, state: &Value) {
        team_agent::state::projection::save_team_scoped_state(&self.run_workspace, state)
            .expect("save fixture runtime state");
    }

    fn target_paths(&self, target: &str) -> team_agent::lifecycle::launch::pi_mcp::PiSeatPaths {
        pi_seat_paths(&self.run_workspace, &self.team_key, target)
    }

    fn transport(&self) -> OfflineTransport {
        OfflineTransport::new()
            .with_session_present(true)
            .with_windows(vec![team_agent::transport::WindowName::new(SOURCE)])
    }

    fn fork(
        &self,
        target: &str,
        label: Option<&str>,
        transport: &dyn team_agent::transport::Transport,
    ) -> Result<team_agent::lifecycle::ForkAgentReport, team_agent::lifecycle::LifecycleError> {
        fork_agent_with_transport(
            &self.team_dir,
            &AgentId::new(SOURCE),
            &AgentId::new(target),
            label,
            false,
            Some(&self.team_key),
            transport,
        )
    }

    fn assert_no_target(&self, target: &str, transport: &OfflineTransport) {
        let state = self.state();
        assert!(
            !state["agents"].get(target).is_some(),
            "rejected fork must not register a usable target; state={state}"
        );
        let paths = self.target_paths(target);
        assert!(
            !paths.wrapper.exists(),
            "rejected fork must not leave a usable wrapper"
        );
        assert!(
            !transport
                .spawn_records()
                .iter()
                .any(|(_, argv)| argv.iter().any(|arg| arg.contains(target))),
            "rejected fork must not spawn a target pane"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn append_session_entry(path: &Path, entry: Value) {
    let mut bytes = fs::read(path).expect("read session before append");
    bytes.extend_from_slice(entry.to_string().as_bytes());
    bytes.push(b'\n');
    fs::write(path, bytes).expect("append independent session entry");
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).expect("file metadata"))
}

fn valid_v3_body() -> Vec<u8> {
    // Stable one-seed generated branch keeps property inputs reproducible.
    let branch_id = format!(
        "branch-{:016x}",
        PROPERTY_SEED ^ PROPERTY_SEED.rotate_left(17)
    );
    let entries = [
        json!({
            "type": "message", "id": "entry-user", "parentId": null,
            "timestamp": "2025-01-02T03:04:06.000Z",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "共同历史：雪だるま 🛰️; session A is ordinary body text."},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"}
            ]}
        }),
        json!({
            "type": "message", "id": "entry-tool-call", "parentId": "entry-user",
            "timestamp": "2025-01-02T03:04:07.000Z",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Inspect the captured fixture."},
                {"type": "toolCall", "id": "tool-1", "name": "read_file", "arguments": {"path": "notes/Δ.md"}}
            ], "provider": "fixture-provider", "opaqueSignature": "signed/東京/🔒"}
        }),
        json!({
            "type": "tool_execution_start", "id": "entry-tool-start", "parentId": "entry-tool-call",
            "timestamp": "2025-01-02T03:04:08.000Z", "toolCallId": "tool-1",
            "toolName": "read_file", "args": {"path": "notes/Δ.md"}
        }),
        json!({
            "type": "tool_execution_end", "id": "entry-tool-result", "parentId": "entry-tool-start",
            "timestamp": "2025-01-02T03:04:09.000Z", "toolCallId": "tool-1",
            "toolName": "read_file", "result": [{"type": "text", "text": "result: preserve me exactly"}],
            "isError": false
        }),
        json!({
            "type": "message", "id": "entry-system-patch", "parentId": "entry-tool-result",
            "timestamp": "2025-01-02T03:04:10.000Z",
            "message": {"role": "system", "content": [{"type": "text", "text": "System patch: retain role-body and profile context."}]}
        }),
        json!({
            "type": "custom", "id": "entry-custom", "parentId": "entry-system-patch",
            "timestamp": "2025-01-02T03:04:11.000Z", "customType": "context_edit",
            "data": {"text": "custom context edit: 東京", "signed": "do-not-rewrite-A"}
        }),
        json!({
            "type": "compaction", "id": "entry-compaction", "parentId": "entry-custom",
            "timestamp": "2025-01-02T03:04:12.000Z", "summary": "Keep the complete branch ancestry.",
            "firstKeptEntryId": "entry-system-patch", "tokensBefore": 1234
        }),
        json!({
            "type": "message", "id": branch_id, "parentId": "entry-system-patch",
            "timestamp": "2025-01-02T03:04:13.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "alternate branch answer"}]}
        }),
    ];
    let mut body = Vec::new();
    for entry in entries {
        body.extend_from_slice(entry.to_string().as_bytes());
        body.push(b'\n');
    }
    body
}

fn write_session(path: &Path, id: &str, cwd: &Path, timestamp: &str, body: &[u8]) {
    fs::create_dir_all(path.parent().expect("session file parent")).expect("create session parent");
    let header = json!({
        "type": "session", "version": 3, "id": id,
        "timestamp": timestamp, "cwd": cwd.to_string_lossy()
    });
    let mut bytes = header.to_string().into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(body);
    fs::write(path, bytes).expect("write session fixture");
}

fn read_header(path: &Path) -> Value {
    let bytes = fs::read(path).expect("read session file");
    let first_line_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("header newline");
    serde_json::from_slice(&bytes[..first_line_end]).expect("valid session header JSON")
}

fn body_bytes(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).expect("read session file");
    let first_line_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("header newline");
    bytes[first_line_end + 1..].to_vec()
}

fn assert_valid_entry_tree(body: &[u8]) {
    let mut ids = std::collections::BTreeSet::new();
    for (index, line) in body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let entry: Value = serde_json::from_slice(line).expect("entry must be valid JSON");
        let id = entry["id"].as_str().expect("entry id");
        assert!(ids.insert(id.to_string()), "entry ids must be unique: {id}");
        if !entry["parentId"].is_null() {
            let parent = entry["parentId"].as_str().expect("parent id string");
            assert!(
                ids.contains(parent),
                "entry {index} parent must precede it: {parent}"
            );
        }
    }
    assert!(
        ids.len() >= 8,
        "v3 fixture includes messages, tool results, and branches"
    );
}

fn role_field<'a>(role: &'a str, key: &str) -> Option<&'a str> {
    let mut in_front_matter = false;
    for line in role.lines() {
        if line.trim() == "---" {
            if in_front_matter {
                break;
            }
            in_front_matter = true;
            continue;
        }
        if in_front_matter {
            if let Some((field, value)) = line.split_once(':') {
                if field.trim() == key {
                    return Some(value.trim());
                }
            }
        }
    }
    None
}

fn role_body(role: &str) -> &str {
    let first = role.find("---").expect("role front matter opener") + 3;
    let second = role[first..].find("---").expect("role front matter closer") + first + 3;
    role[second..].trim_start_matches(&['\r', '\n'][..])
}

fn forked_target_file(fixture: &Fixture, state: &Value, target: &str) -> PathBuf {
    let path = state["agents"][target]["rollout_path"]
        .as_str()
        .expect("successful fork registers target rollout_path");
    PathBuf::from(path)
}

#[test]
#[serial(env)]
fn r01_public_lifecycle_port_pi_fork_must_create_a_distinct_seat() {
    let fixture = Fixture::new();
    let source = fixture.state()["agents"][SOURCE].clone();
    assert_eq!(source["provider"], "pi");
    assert_eq!(source["session_id"], SESSION_A);
    assert!(fixture.source_file.is_file());
    assert_valid_entry_tree(&fixture.source_body);

    let response = team_agent::cli::lifecycle_port::fork_agent(
        &fixture.team_dir,
        SOURCE,
        TARGET,
        None,
        Some(&fixture.team_key),
    )
    .expect("public lifecycle port returns its JSON result");
    assert_eq!(
        response["ok"], true,
        "R01: valid Pi fork-agent SOURCE --as TARGET must create a managed new seat through the public lifecycle port; baseline currently refuses Pi native session fork. response={response}"
    );
    assert_eq!(response["source_agent_id"], SOURCE);
    assert_eq!(response["new_agent_id"], TARGET);
    assert_ne!(response["session_id"], SESSION_A);
    let after = fixture.state();
    assert!(
        after["agents"].get(TARGET).is_some(),
        "public fork must register TARGET"
    );
}

#[test]
#[serial(env)]
fn r02_r03_fork_is_an_independent_identity_with_the_exact_complete_v3_tree() {
    let fixture = Fixture::new();
    let transport = fixture.transport();
    let source_before = fs::read(&fixture.source_file).expect("source backing before fork");
    #[cfg(unix)]
    let source_inode = inode(&fixture.source_file);
    let report = fixture
        .fork(TARGET, None, &transport)
        .expect("R02/R03: valid Pi session must fork instead of injecting into SOURCE");
    assert_eq!(report.source_agent_id.as_str(), SOURCE);
    assert_eq!(report.new_agent_id.as_str(), TARGET);
    let new_session_id = report
        .session_id
        .as_ref()
        .expect("fork session id")
        .as_str();
    assert_ne!(
        new_session_id, SESSION_A,
        "TARGET gets a fresh session UUID"
    );
    assert_eq!(
        report.backing_state,
        team_agent::lifecycle::ForkBackingState::Verified
    );

    let state = fixture.state();
    let source = &state["agents"][SOURCE];
    let target = &state["agents"][TARGET];
    assert_eq!(
        source["session_id"], SESSION_A,
        "SOURCE tuple remains untouched"
    );
    assert_eq!(
        source["rollout_path"],
        fixture.source_file.to_string_lossy().as_ref()
    );
    assert_eq!(target["session_id"], new_session_id);
    assert_eq!(target["provider"], "pi");
    assert!(target["pane_id"].as_str().is_some());
    assert_ne!(
        target["pane_id"], source["pane_id"],
        "new seat has its own pane"
    );

    let target_paths = fixture.target_paths(TARGET);
    assert_ne!(
        fs::canonicalize(&fixture.source_paths.runtime_root).expect("canonical source root"),
        fs::canonicalize(&target_paths.runtime_root).expect("canonical target root"),
        "source and target canonical seat roots differ"
    );
    let target_file = forked_target_file(&fixture, &state, TARGET);
    assert!(target_file.starts_with(&target_paths.sessions));
    assert_ne!(
        fs::canonicalize(&fixture.source_file).expect("canonical source file"),
        fs::canonicalize(&target_file).expect("canonical target file"),
        "source and target backing paths differ"
    );
    #[cfg(unix)]
    assert_ne!(
        source_inode,
        inode(&target_file),
        "target backing must be a distinct inode, not a hard link"
    );
    assert_eq!(
        fs::read(&fixture.source_file).expect("source remains"),
        source_before
    );
    let header = read_header(&target_file);
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);
    assert_eq!(header["id"], new_session_id);
    assert_ne!(header["id"], SESSION_A);
    assert_eq!(
        header["parentSession"],
        fixture.source_file.to_string_lossy().as_ref(),
        "new Pi header records the exact source backing as parentSession"
    );
    assert_eq!(
        body_bytes(&target_file),
        fixture.source_body,
        "non-header bytes are exact"
    );
    assert_valid_entry_tree(&body_bytes(&target_file));

    let entries: Vec<Value> = body_bytes(&target_file)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("copied entry JSON"))
        .collect();
    assert!(
        entries
            .iter()
            .any(|entry| entry["type"] == "tool_execution_end")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["customType"] == "context_edit")
    );
    assert!(entries.iter().any(|entry| entry["type"] == "compaction"));
    let spawn_args = transport
        .spawn_records()
        .into_iter()
        .flat_map(|(_, argv)| argv)
        .collect::<Vec<_>>();
    assert!(
        spawn_args
            .iter()
            .any(|arg| arg.contains(target_file.to_string_lossy().as_ref())),
        "new pane resumes only the new backing G; argv={spawn_args:?}"
    );
    assert!(
        !spawn_args
            .iter()
            .any(|arg| arg.contains(fixture.source_file.to_string_lossy().as_ref())),
        "new pane must not resume SOURCE backing F"
    );
    let source_pane = source["pane_id"].as_str().unwrap_or("");
    assert!(
        transport.inject_targets().iter().all(|target| {
            source_pane.is_empty() || !format!("{target:?}").contains(source_pane)
        }),
        "SOURCE must not receive injected commands"
    );
    assert!(
        !transport
            .calls()
            .iter()
            .any(|call| call.contains("kill") || call.contains("restart")),
        "SOURCE must not receive kill/restart during fork"
    );
    assert!(
        target_paths.wrapper.is_file(),
        "TARGET owns a fresh Pi wrapper"
    );
    let wrapper = fs::read_to_string(&target_paths.wrapper).expect("read target wrapper");
    assert!(
        wrapper.contains(TARGET),
        "wrapper identity belongs to TARGET"
    );
    assert!(
        source_pane.is_empty() || !wrapper.contains(source_pane),
        "wrapper must not inherit SOURCE pane identity"
    );
}

#[test]
#[serial(env)]
fn r04_target_materializes_the_latest_role_changing_only_identity_and_label() {
    let fixture = Fixture::new();
    let transport = fixture.transport();
    fixture
        .fork(TARGET, Some("Variant Researcher"), &transport)
        .expect("R04: valid Pi source role must materialize on fork");
    let state = fixture.state();
    let source_path = PathBuf::from(
        state["agents"][SOURCE]["dynamic_role_file"]
            .as_str()
            .unwrap(),
    );
    let target_path = PathBuf::from(
        state["agents"][TARGET]["dynamic_role_file"]
            .as_str()
            .unwrap(),
    );
    let source_role = fs::read_to_string(source_path).expect("read latest source role");
    let target_role = fs::read_to_string(target_path).expect("read materialized target role");
    assert_eq!(role_field(&target_role, "name"), Some(TARGET));
    assert_eq!(role_field(&target_role, "role"), Some("Variant Researcher"));
    for field in [
        "provider",
        "model",
        "auth_mode",
        "effort",
        "dangerously_skip_permissions",
        "communication_mode",
        "profile",
    ] {
        assert_eq!(
            role_field(&target_role, field),
            role_field(&source_role, field),
            "role field {field} is inherited"
        );
    }
    assert!(
        target_role.contains("mcp_team"),
        "tools and permissions are preserved"
    );
    assert_eq!(
        role_body(&target_role),
        role_body(&source_role),
        "role body bytes stay intact"
    );
    assert_eq!(state["agents"][TARGET]["provider"], "pi");
}

#[test]
#[serial(env)]
fn r05_bad_source_tuples_and_jsonl_fail_closed_without_a_fresh_target() {
    let fixture = Fixture::new();
    let original_state = fixture.state();
    let original_bytes = fs::read(&fixture.source_file).expect("source bytes");
    let outside = fixture.root.join("outside.jsonl");
    write_session(
        &outside,
        SESSION_A,
        &fixture.run_workspace,
        SESSION_TIMESTAMP,
        &fixture.source_body,
    );

    let mut cases = vec![
        "missing-tuple",
        "wrong-header-id",
        "wrong-cwd",
        "wrong-version",
        "wrong-timestamp",
        "bad-json",
        "broken-parent",
        "truncated",
        "truncated-json",
        "outside-root",
    ];
    #[cfg(unix)]
    cases.push("symlink");
    for case in cases {
        let mut state = original_state.clone();
        let source = state["agents"][SOURCE].as_object_mut().expect("source row");
        let mut bytes = original_bytes.clone();
        match case {
            "missing-tuple" => {
                source.remove("rollout_path");
            }
            "wrong-header-id" => {
                write_session(
                    &fixture.source_file,
                    "cc09d133-878f-4241-920f-f7ebfefc2a9e",
                    &fixture.run_workspace,
                    SESSION_TIMESTAMP,
                    &fixture.source_body,
                );
            }
            "wrong-cwd" => {
                let wrong_cwd = fixture.root.join("wrong-cwd");
                write_session(
                    &fixture.source_file,
                    SESSION_A,
                    &wrong_cwd,
                    SESSION_TIMESTAMP,
                    &fixture.source_body,
                );
            }
            "wrong-version" => {
                let header = json!({"type":"session","version":2,"id":SESSION_A,"timestamp":SESSION_TIMESTAMP,"cwd":fixture.run_workspace.to_string_lossy()});
                bytes = [header.to_string().as_bytes(), b"\n", &fixture.source_body].concat();
                fs::write(&fixture.source_file, &bytes).expect("write wrong-version session");
            }
            "wrong-timestamp" => {
                let header = json!({"type":"session","version":3,"id":SESSION_A,"timestamp":"not-a-timestamp","cwd":fixture.run_workspace.to_string_lossy()});
                bytes = [header.to_string().as_bytes(), b"\n", &fixture.source_body].concat();
                fs::write(&fixture.source_file, &bytes).expect("write invalid-timestamp session");
            }
            "bad-json" => {
                bytes = [b"{not-json}\n".as_slice(), &fixture.source_body].concat();
                fs::write(&fixture.source_file, &bytes).expect("write malformed source");
            }
            "broken-parent" => {
                let invalid = json!({"type":"message","id":"orphan","parentId":"missing-entry","message":{"role":"user","content":[]}}).to_string();
                bytes = [serde_json::to_string(&json!({"type":"session","version":3,"id":SESSION_A,"timestamp":SESSION_TIMESTAMP,"cwd":fixture.run_workspace.to_string_lossy()})).unwrap().as_bytes(), b"\n", invalid.as_bytes(), b"\n"].concat();
                fs::write(&fixture.source_file, &bytes).expect("write invalid entry tree");
            }
            "truncated" => {
                bytes.pop();
                fs::write(&fixture.source_file, &bytes).expect("truncate final JSONL newline");
            }
            "truncated-json" => {
                let last_line = bytes[..bytes.len() - 1]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .expect("last entry begins after a newline")
                    + 1;
                bytes.truncate(last_line + 3);
                fs::write(&fixture.source_file, &bytes).expect("truncate inside final JSON entry");
            }
            "outside-root" => {
                source.insert(
                    "rollout_path".to_string(),
                    json!(outside.to_string_lossy().to_string()),
                );
            }
            #[cfg(unix)]
            "symlink" => {
                fs::remove_file(&fixture.source_file).expect("remove source for symlink case");
                std::os::unix::fs::symlink(&outside, &fixture.source_file)
                    .expect("create source symlink");
            }
            _ => unreachable!(),
        }
        fixture.save_state(&state);
        let transport = fixture.transport();
        let result = fixture.fork(TARGET, None, &transport);
        assert!(
            result.is_err(),
            "R05 ({case}): invalid source must refuse, never fresh-fork; result={result:?}"
        );
        fixture.assert_no_target(TARGET, &transport);
        let after = fixture.state();
        assert!(
            after["agents"].get(TARGET).is_none(),
            "R05 ({case}): no target roster row"
        );
        if case != "missing-tuple" && case != "outside-root" {
            let source_after = &after["agents"][SOURCE];
            assert_eq!(
                source_after["session_id"], SESSION_A,
                "source identity remains unchanged"
            );
        }
        if case == "symlink" {
            fs::remove_file(&fixture.source_file).expect("remove source symlink");
        }
        fs::write(&fixture.source_file, &original_bytes)
            .expect("restore valid source between adversarial cases");
    }
    fixture.save_state(&original_state);
    fixture.assert_valid_source_fixture();
}

#[test]
#[serial(env)]
fn r06_target_collisions_preserve_existing_roster_spec_and_resource_bytes() {
    let fixture = Fixture::new();
    let mut state = fixture.state();
    state["agents"][TARGET] = json!({"status":"stopped","provider":"pi","sentinel":"must-survive"});
    fixture.save_state(&state);
    let roster_before = fs::read(&fixture.state_path).expect("persisted roster bytes");
    let selected = team_agent::state::selector::resolve_active_team(
        &fixture.team_dir,
        Some(&fixture.team_key),
        team_agent::state::selector::SelectorMode::RequireSpec,
    )
    .expect("selected team");
    let spec_path = selected.spec_path.expect("compiled spec path");
    let spec_before = fs::read(&spec_path).expect("spec bytes");
    let target_paths = fixture.target_paths(TARGET);
    fs::create_dir_all(&target_paths.sessions).expect("pre-existing target resource");
    let sentinel = target_paths.sessions.join("keep.jsonl");
    fs::write(&sentinel, b"collision bytes\n").expect("write collision sentinel");
    let transport = fixture.transport();
    assert!(
        fixture.fork(TARGET, None, &transport).is_err(),
        "R06 duplicate target must refuse"
    );
    assert_eq!(
        fs::read(&fixture.state_path).unwrap(),
        roster_before,
        "roster bytes stay unchanged"
    );
    assert_eq!(
        fs::read(&spec_path).unwrap(),
        spec_before,
        "compiled spec bytes stay unchanged"
    );
    assert_eq!(
        fs::read(sentinel).unwrap(),
        b"collision bytes\n",
        "existing target bytes are never overwritten"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "collision must not spawn a second target"
    );
}

#[test]
#[serial(env)]
fn r07_capture_observation_does_not_replace_the_inherited_target_tuple() {
    let fixture = Fixture::new();
    let transport = fixture.transport();
    fixture.fork(TARGET, None, &transport).expect(
        "R07: source header predates spawn, but the captured source tuple is authoritative",
    );
    let mut state = fixture.state();
    let target_before = state["agents"][TARGET].clone();
    let mut adapter_for = |provider| get_adapter(provider);
    let report = team_agent::provider::session::capture::capture_missing_provider_sessions_once(
        &mut state,
        &mut adapter_for,
        true,
        1,
    )
    .expect("coordinator observation merge");
    assert!(
        report.assigned.is_empty(),
        "complete inherited tuples are not recaptured: {report:?}"
    );
    assert_eq!(
        state["agents"][TARGET]["session_id"],
        target_before["session_id"]
    );
    assert_eq!(
        state["agents"][TARGET]["rollout_path"],
        target_before["rollout_path"]
    );
    fixture.save_state(&state);
    let reloaded = fixture.state();
    assert_eq!(
        reloaded["agents"][TARGET]["session_id"], target_before["session_id"],
        "persisted observation merge keeps inherited session B"
    );
    assert_eq!(
        reloaded["agents"][TARGET]["rollout_path"], target_before["rollout_path"],
        "persisted observation merge keeps backing G"
    );
    assert_eq!(reloaded["agents"][SOURCE]["session_id"], SESSION_A);
    assert_eq!(
        reloaded["agents"][SOURCE]["rollout_path"],
        fixture.source_file.to_string_lossy().as_ref()
    );
}

#[test]
#[serial(env)]
fn r08_sibling_observation_racing_with_target_spawn_is_not_lost() {
    let fixture = Fixture::new();
    let mut observed = fixture.state();
    observed["agents"]["sibling"] = json!({
        "provider":"pi", "status":"running", "session_id":"sibling-session",
        "rollout_path":"/sibling/only.jsonl", "captured_at":"2026-09-12T10:01:00Z",
        "captured_via":"session_scan", "observation_generation":"concurrent-observation-17"
    });
    let transport = fixture.transport();
    let observed_transport = transport.clone();
    let run_workspace = fixture.run_workspace.clone();
    let updater = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !observed_transport
            .calls()
            .iter()
            .any(|call| call.starts_with("spawn"))
        {
            if Instant::now() >= deadline {
                return Err("valid Pi fork never reached target spawn".to_string());
            }
            std::thread::yield_now();
        }
        team_agent::state::projection::save_team_scoped_state(&run_workspace, &observed)
            .map_err(|error| format!("persist concurrent sibling observation: {error}"))
    });
    let fork_result = fixture.fork(TARGET, None, &transport);
    let update_result = updater.join().expect("sibling observation thread");
    assert!(
        update_result.is_ok(),
        "R08 concurrent state update must commit while target spawn is in flight; fork={fork_result:?} update={update_result:?}"
    );
    let fork_report =
        fork_result.expect("R08 Pi fork must complete after the concurrent sibling observation");
    assert_eq!(fork_report.new_agent_id.as_str(), TARGET);
    let after = fixture.state();
    assert_eq!(
        after["agents"]["sibling"]["observation_generation"], "concurrent-observation-17",
        "fork must merge its target row, not roll back an observation concurrent with target spawn"
    );
    assert_eq!(after["agents"][SOURCE]["session_id"], SESSION_A);
    assert!(after["agents"].get(TARGET).is_some());
}

#[test]
#[serial(env)]
fn r09_spawn_failure_is_compensated_without_an_alive_target_on_deleted_backing() {
    let fixture = Fixture::new();
    let transport = fixture
        .transport()
        .with_spawn_failure(TARGET, "R09 injected target spawn failure");
    let result = fixture.fork(TARGET, None, &transport);
    assert!(
        result.is_err(),
        "R09 injected spawn failure must be reported: {result:?}"
    );
    assert!(
        transport.calls().iter().any(|call| call.contains("spawn")),
        "R09 setup must reach the existing transport spawn fault-injection seam; calls={:?}",
        transport.calls()
    );
    let state = fixture.state();
    assert!(
        !state["agents"]
            .get(TARGET)
            .is_some_and(|target| target["status"] == "running"),
        "failed target spawn cannot be reported as alive; state={state}"
    );
    let target_paths = fixture.target_paths(TARGET);
    if !target_paths.sessions.exists() {
        assert!(
            !target_paths.wrapper.exists(),
            "fully rolled-back failure leaves no wrapper"
        );
    }
    assert_eq!(state["agents"][SOURCE]["session_id"], SESSION_A);
    assert_eq!(
        state["agents"][SOURCE]["rollout_path"],
        fixture.source_file.to_string_lossy().as_ref()
    );
}

#[test]
#[serial(env)]
fn r09_pane_verification_failure_compensates_the_new_target() {
    let fixture = Fixture::new();
    let transport = fixture.transport().with_spawned_panes_addressable(false);
    let result = fixture.fork(TARGET, None, &transport);
    assert!(
        result.is_err(),
        "R09 pane verification failure must be reported: {result:?}"
    );
    assert!(
        transport
            .calls()
            .iter()
            .any(|call| call.starts_with("spawn")),
        "R09 setup must reach the transport spawn seam before pane verification; calls={:?}",
        transport.calls()
    );
    assert!(
        transport.calls().contains(&"kill_pane"),
        "R09 must compensate the spawned target pane after verification fails; calls={:?}",
        transport.calls()
    );
    let state = fixture.state();
    assert!(
        !state["agents"]
            .get(TARGET)
            .is_some_and(|target| target["status"] == "running"),
        "unverified target cannot remain alive in runtime state; state={state}"
    );
    let target_paths = fixture.target_paths(TARGET);
    assert!(
        !target_paths.wrapper.exists(),
        "failed target must not retain a runnable Pi wrapper"
    );
    assert_eq!(state["agents"][SOURCE]["session_id"], SESSION_A);
    assert_eq!(
        state["agents"][SOURCE]["rollout_path"],
        fixture.source_file.to_string_lossy().as_ref()
    );
}

#[test]
#[serial(env)]
fn r10_messages_remain_team_scoped_and_target_messages_name_only_target() {
    let fixture = Fixture::new();
    let store = team_agent::db::message_store::MessageStore::open(&fixture.run_workspace)
        .expect("open selected team's message database");
    let source_marker = store
        .create_message_with_id(
            "msg_fork_source_keep",
            None,
            "caller",
            SOURCE,
            "source-only",
            None,
            false,
            Some(&fixture.team_key),
        )
        .expect("seed source message marker");
    let sibling_marker = store
        .create_message_with_id(
            "msg_fork_sibling_keep",
            None,
            "caller",
            "sibling",
            "sibling-only",
            None,
            false,
            Some("sibling-team"),
        )
        .expect("seed sibling-team message marker");
    let transport = fixture.transport();
    fixture
        .fork(TARGET, None, &transport)
        .expect("R10 database ownership checks require a successfully registered target");
    let target_marker = store
        .create_message_with_id(
            "msg_fork_target_owner",
            None,
            SOURCE,
            TARGET,
            "target-only",
            None,
            false,
            Some(&fixture.team_key),
        )
        .expect("target message is stored in selected team DB");
    let connection = team_agent::db::schema::open_db(store.db_path()).expect("read team DB");
    for (id, owner, recipient) in [
        (&source_marker, fixture.team_key.as_str(), SOURCE),
        (&sibling_marker, "sibling-team", "sibling"),
        (&target_marker, fixture.team_key.as_str(), TARGET),
    ] {
        let actual: (Option<String>, String) = connection
            .query_row(
                "select owner_team_id, recipient from messages where message_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("message row exists");
        assert_eq!(
            actual.0.as_deref(),
            Some(owner),
            "message {id} has its owner team"
        );
        assert_eq!(
            actual.1, recipient,
            "message {id} remains attributed to its recipient"
        );
    }
}

#[test]
#[serial(env)]
fn r11_source_and_target_backings_remain_independently_loadable() {
    let fixture = Fixture::new();
    let transport = fixture.transport();
    fixture
        .fork(TARGET, None, &transport)
        .expect("R11 independent lifecycle requires a successful fork");
    let state = fixture.state();
    let target_file = forked_target_file(&fixture, &state, TARGET);
    append_session_entry(
        &fixture.source_file,
        json!({
            "type":"message", "id":"entry-source-live", "parentId":"entry-user",
            "timestamp":"2026-09-12T10:02:00Z",
            "message":{"role":"assistant","content":[{"type":"text","text":"SOURCE-only follow-up"}]}
        }),
    );
    append_session_entry(
        &target_file,
        json!({
            "type":"message", "id":"entry-target-live", "parentId":"entry-user",
            "timestamp":"2026-09-12T10:03:00Z",
            "message":{"role":"assistant","content":[{"type":"text","text":"TARGET-only follow-up"}]}
        }),
    );
    let target_before =
        fs::read(&target_file).expect("target backing bytes after target-only append");
    let source_before =
        fs::read(&fixture.source_file).expect("source backing after source-only append");
    assert!(
        source_before
            .windows(b"SOURCE-only follow-up".len())
            .any(|bytes| bytes == b"SOURCE-only follow-up")
    );
    assert!(
        !source_before
            .windows(b"TARGET-only follow-up".len())
            .any(|bytes| bytes == b"TARGET-only follow-up")
    );
    assert!(
        target_before
            .windows(b"TARGET-only follow-up".len())
            .any(|bytes| bytes == b"TARGET-only follow-up")
    );
    assert!(
        !target_before
            .windows(b"SOURCE-only follow-up".len())
            .any(|bytes| bytes == b"SOURCE-only follow-up")
    );
    assert_valid_entry_tree(&body_bytes(&fixture.source_file));
    assert_valid_entry_tree(&body_bytes(&target_file));
    fs::remove_file(&fixture.source_file).expect("simulate SOURCE backing retirement");
    assert_eq!(
        fs::read(&target_file).expect("TARGET remains offline-loadable"),
        target_before
    );
    fs::write(&fixture.source_file, &source_before).expect("restore independent SOURCE backing");
    fs::remove_file(&target_file).expect("simulate TARGET backing retirement");
    assert!(
        fixture.source_file.is_file(),
        "removing TARGET backing must not remove SOURCE backing"
    );
    assert_eq!(fs::read(&fixture.source_file).unwrap(), source_before);
}

#[test]
#[serial(env)]
fn r12_pi_clone_and_non_pi_in_window_fork_compatibility_are_preserved() {
    let workspace = Path::new("/workspace/pi-fork-contract");
    let source_paths = pi_seat_paths(workspace, "team-a", SOURCE);
    let clone_paths = pi_seat_paths(workspace, "team-a", TARGET);
    assert_ne!(source_paths.runtime_root, clone_paths.runtime_root);
    assert_ne!(source_paths.wrapper, clone_paths.wrapper);
    assert_ne!(source_paths.sessions, clone_paths.sessions);
    assert!(
        matches!(
            get_adapter(Provider::Pi).fork(
                Some(&SessionId::new(SESSION_A)),
                AuthMode::Subscription,
                None
            ),
            Err(team_agent::provider::ProviderError::CapabilityUnsupported(
                _
            ))
        ),
        "managed fork must not be faked by changing the provider's unsupported native-fork capability"
    );
    assert_eq!(
        team_agent::lifecycle::launch::in_window_fork(Provider::Grok, AuthMode::Subscription)
            .map(|spec| spec.command),
        Some("/fork"),
        "non-Pi Grok subscription keeps its verified in-window command"
    );
    assert_eq!(
        team_agent::lifecycle::launch::in_window_fork(Provider::ClaudeCode, AuthMode::Subscription)
            .map(|spec| spec.command),
        Some("/branch"),
        "non-Pi Claude subscription keeps its verified in-window command"
    );
    assert!(
        team_agent::lifecycle::launch::in_window_fork(Provider::Pi, AuthMode::Subscription)
            .is_none(),
        "Pi must use managed new-seat inheritance, never an in-window slash alias"
    );
    let fixture = Fixture::new();
    let state = fixture.state();
    assert_eq!(state["agents"][SOURCE]["session_id"], SESSION_A);
    assert!(
        fixture
            .source_paths
            .sessions
            .starts_with(&fixture.source_paths.runtime_root)
    );
}
