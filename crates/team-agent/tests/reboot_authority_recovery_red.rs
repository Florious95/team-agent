#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![cfg(unix)]
//! RED suite: an OS reboot (or macOS tmp_cleaner) must never brick an existing team.
//!
//! Field incident 2026-11-10, team codex-effort-fix: after a reboot every status, doctor,
//! start and `team restart --allow-fresh` failed with `native_authority_lost: /private/tmp/ta-…`.
//! The team's framework binding still pointed its authority at the pre-168f99005 temp root.
//!
//! Every fixture here reproduces on-disk data written by released 0.5.12x binaries:
//! - a version-1 `framework-binding.json`;
//! - its st_dev-bearing workspace identity digest and the scope id derived from it;
//! - an authority root of the legacy shape `/private/tmp/ta-<24 hex>`.
//! A "reboot" kills the test-owned tmux servers, then deletes that temp root (or only its regular
//! files, like tmp_cleaner). Tests are `r_*` (RED on the baseline) or `g_*` (guards that must stay
//! green). Every step is logged with argv, exit code, stdout, stderr and the on-disk binding state.

#[path = "e2e/framework.rs"]
mod framework;
#[path = "../../team-agent-contract/tests/physical_support/mod.rs"]
mod physical_support;

/// Re-executed by the controlled terminal fixture (r8); not an acceptance case.
#[test]
#[ignore = "internal owned terminal child, not a skipped acceptance case"]
fn native_terminal_fixture() {
    physical_support::native_terminal::run();
}

use framework::{run_ta, TaResult, TestWorkspace};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use team_agent_contract::{contract::types::ScopeId, orchestration::store::ProjectionStore};

const WORKER: &str = "w1";
/// Contract-family seat that was explicitly stopped and never instantiated (field: base-sol).
const NATIVE: &str = "keeper";
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn log(step: &str, detail: impl std::fmt::Display) {
    eprintln!("[reboot-red] {step}: {detail}");
}

fn clip(text: &str) -> String {
    const MAX: usize = 6000;
    if text.len() <= MAX {
        return text.to_string();
    }
    let mut end = MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[{} more bytes]", &text[..end], text.len() - end)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn ws_str(ws: &TestWorkspace) -> String {
    ws.path().to_str().unwrap().to_string()
}

fn ta(ws: &TestWorkspace, args: &[&str]) -> TaResult {
    let out = run_ta(ws, args);
    log(
        "command",
        format!(
            "{:?}\n  exit={}\n  stdout={}\n  stderr={}",
            out.argv,
            out.exit_code,
            clip(&out.stdout),
            clip(&out.stderr)
        ),
    );
    out
}

fn transcript(out: &TaResult) -> String {
    format!("argv={:?} exit={} stdout={} stderr={}", out.argv, out.exit_code, clip(&out.stdout), clip(&out.stderr))
}

/// A legacy temp authority root created by this test only; removed on drop.
struct TempRoot(PathBuf);
impl TempRoot {
    fn fresh() -> Self {
        let tmp = Path::new("/tmp").canonicalize().unwrap();
        let seed = format!(
            "{}-{}-{:?}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
        );
        let path = tmp.join(format!("ta-{}", &hex(&Sha256::digest(seed.as_bytes()))[..24]));
        assert!(!path.exists(), "refusing to reuse an existing temp root {}", path.display());
        Self(path)
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tmux_path() -> PathBuf {
    let out = std::process::Command::new("sh").args(["-c", "command -v tmux"]).output().unwrap();
    assert!(out.status.success(), "tmux is required");
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim()).canonicalize().unwrap()
}

/// Workspace identity exactly as released binaries computed it (st_dev included).
fn legacy_identity(path: &Path, dev: u64, ino: u64) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(b"team-agent-contract-cwd-v1\0");
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.update(dev.to_le_bytes());
    hash.update(ino.to_le_bytes());
    hash.finalize().to_vec()
}

struct LegacyBinding {
    scope: String,
    dir: PathBuf,
    transport_root: PathBuf,
}

/// Write a released-format binding for (workspace, team). `dev_xor` != 0 simulates a binding
/// written on an earlier boot, when macOS had numbered the data volume differently.
fn write_legacy_binding(ws: &TestWorkspace, team: &str, recorded_path: &Path, dev_xor: u64, transport_root: &Path) -> LegacyBinding {
    let real = ws.path().canonicalize().unwrap();
    let meta = fs::metadata(&real).unwrap();
    let identity = legacy_identity(recorded_path, meta.dev() ^ dev_xor, meta.ino());
    // serde field order of CwdIdentity is (path, identity); Value maps would sort keys.
    let encoded = format!(
        "[{{\"path\":{},\"identity\":{}}},{}]",
        serde_json::to_string(recorded_path.to_str().unwrap()).unwrap(),
        serde_json::to_string(&identity).unwrap(),
        serde_json::to_string(team).unwrap()
    );
    let scope = format!("contract-{}", &hex(&Sha256::digest(encoded.as_bytes()))[..32]);
    let dir = real.join(".team/runtime/contract").join(&scope);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).unwrap();
    let sut = framework::ta_binary().canonicalize().unwrap();
    let binding = json!({
        "version": 1,
        "workspace": {"path": recorded_path, "identity": identity},
        "team": team,
        "scope": scope,
        "endpoint": format!("framework:{scope}"),
        "candidate": sut,
        "candidate_sha256": Sha256::digest(fs::read(&sut).unwrap()).to_vec(),
        "tmux": tmux_path(),
        "transport_root": transport_root,
    });
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("framework-binding.json"))
        .unwrap();
    file.write_all(&serde_json::to_vec(&binding).unwrap()).unwrap();
    file.sync_all().unwrap();
    log("fixture", format!("legacy binding {} -> transport_root {}", dir.display(), transport_root.display()));
    LegacyBinding { scope, dir, transport_root: transport_root.to_path_buf() }
}

/// A live authority store at `root`, plus an opaque native evidence file (like
/// `native-<hash>/launch.sh`) that must survive any relocation. Returns (relative path, bytes).
fn create_store(binding: &LegacyBinding) -> (PathBuf, Vec<u8>) {
    let scope = ScopeId::new(binding.scope.clone()).unwrap();
    drop(ProjectionStore::create(&binding.transport_root, scope, &format!("framework:{}", binding.scope)).unwrap());
    let rel = PathBuf::from("native-5e7f1a2b3c4d5e6f/launch.sh");
    let bytes = format!("#!/bin/sh\n# preserved native evidence {}\n", binding.scope).into_bytes();
    let evidence = binding.transport_root.join(&rel);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(evidence.parent().unwrap()).unwrap();
    fs::write(&evidence, &bytes).unwrap();
    log("fixture", format!("authority store at {} (+ evidence {})", binding.transport_root.display(), rel.display()));
    (rel, bytes)
}

/// Every SUT-created tmux endpoint named in state (`ta-*` sockets only, never the ambient server).
fn sut_sockets(ws: &TestWorkspace) -> Vec<PathBuf> {
    let Ok(raw) = fs::read_to_string(ws.state_json_path()) else { return Vec::new() };
    let state: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut found = vec![state["tmux_socket"].clone()];
    if let Some(teams) = state["teams"].as_object() {
        found.extend(teams.values().map(|team| team["tmux_socket"].clone()));
    }
    let mut out: Vec<PathBuf> = found
        .iter()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ta-")) && p.exists())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn own_sockets(ws: &TestWorkspace) {
    for socket in sut_sockets(ws) {
        ws.register_owned_tmux_socket(&socket);
    }
}

#[derive(Clone, Copy, Debug)]
enum Purge {
    /// Reboot: the whole temp root is gone.
    Whole,
    /// macOS tmp_cleaner: directories survive, every regular file is deleted.
    RegularFiles,
}

fn purge_regular_files(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            purge_regular_files(&path);
        } else {
            fs::remove_file(&path).unwrap();
        }
    }
}

/// Kill the team's tmux servers (all panes and workers die), then purge the temp authority.
fn reboot(ws: &TestWorkspace, temp_root: Option<&Path>, purge: Purge) {
    own_sockets(ws);
    for socket in sut_sockets(ws) {
        let out = std::process::Command::new("tmux").arg("-S").arg(&socket).arg("kill-server").output().unwrap();
        log("reboot", format!("kill-server {} -> {}", socket.display(), out.status));
    }
    if let Some(root) = temp_root {
        match purge {
            Purge::Whole => {
                let _ = fs::remove_dir_all(root);
            }
            Purge::RegularFiles => {
                if root.exists() {
                    purge_regular_files(root);
                }
            }
        }
        log("reboot", format!("purged {} ({purge:?}); exists afterwards={}", root.display(), root.exists()));
    }
}

/// A team whose fake legacy worker ran before the reboot, plus one contract-family row that was
/// explicitly stopped and never instantiated (no contract_instance), exactly like base-sol.
fn team(tag: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(tag).with_fake_spec(&[WORKER]);
    let dir = ws_str(&ws);
    let started = ta(&ws, &["start", "--from", &dir, "--workspace", &dir, "--team", tag, "--json"]);
    own_sockets(&ws);
    let body: Value = serde_json::from_str(&started.stdout).unwrap_or(Value::Null);
    assert!(started.exit_code == 0 && body["ok"] == json!(true), "fixture team start failed: {}", transcript(&started));
    fs::write(
        ws.path().join("agents").join(format!("{NATIVE}.md")),
        format!("---\nname: {NATIVE}\nrole: Contract seat {NATIVE}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\n---\n\nContract seat.\n"),
    )
    .unwrap();
    let row = json!({
        "agent_id": NATIVE, "provider": "fake", "runtime_family": "contract", "status": "stopped",
        "explicit_stop": true, "model": "fake", "role": NATIVE, "owner_team_id": tag,
    });
    ws.mutate_state(|state| {
        if let Some(agents) = state.pointer_mut(&format!("/teams/{tag}/agents")).and_then(Value::as_object_mut) {
            agents.insert(NATIVE.into(), row.clone());
        }
        let active = state["active_team_key"].as_str() == Some(tag) || state["team_key"].as_str() == Some(tag);
        if active {
            if let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) {
                agents.insert(NATIVE.into(), row.clone());
            }
        }
    });
    log("fixture", format!("team {tag} at {} with legacy {WORKER} and contract row {NATIVE}", ws.path().display()));
    ws
}

/// All bindings for `team` in this workspace, as (directory, parsed JSON).
fn bindings(ws: &TestWorkspace, team: &str) -> Vec<(PathBuf, Value)> {
    let root = ws.path().join(".team/runtime/contract");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let file = entry.path().join("framework-binding.json");
            if let Ok(raw) = fs::read_to_string(&file) {
                let value: Value = serde_json::from_str(&raw).unwrap_or(json!({"unparseable": raw}));
                if value["team"].as_str() == Some(team) {
                    out.push((entry.path(), value));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn tree(path: &Path, depth: usize, out: &mut String) {
    let Ok(entries) = fs::read_dir(path) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for entry in entries {
        out.push_str(&format!("{}{}\n", "  ".repeat(depth), entry.display()));
        if entry.is_dir() && depth < 3 {
            tree(&entry, depth + 1, out);
        }
    }
}

fn disk_state(ws: &TestWorkspace, team: &str) -> String {
    let mut listing = String::new();
    tree(&ws.path().join(".team/runtime/contract"), 0, &mut listing);
    let found: Vec<String> = bindings(ws, team)
        .into_iter()
        .map(|(dir, value)| format!("{} => transport_root={} scope={}", dir.display(), value["transport_root"], value["scope"]))
        .collect();
    format!("bindings for {team}: {found:#?}\ncontract tree:\n{listing}")
}

/// The authority pointer must never again reference a root outside the workspace.
fn assert_authority_inside_workspace(ws: &TestWorkspace, team: &str, old: &Path, step: &str) -> PathBuf {
    let found = bindings(ws, team);
    let state = disk_state(ws, team);
    log(step, &state);
    assert_eq!(found.len(), 1, "{step}: exactly one framework binding must exist for team {team}\n{state}");
    let root = PathBuf::from(found[0].1["transport_root"].as_str().unwrap_or_default());
    assert!(
        root != old && root.starts_with(ws.path()),
        "{step}: the binding still points its authority outside the workspace at {} (legacy temp root {}). \
         It must be copied forward into durable storage under the workspace before the OS purges /tmp.\n{state}",
        root.display(),
        old.display()
    );
    let scope = ScopeId::new(found[0].1["scope"].as_str().unwrap().to_string()).unwrap();
    let endpoint = found[0].1["endpoint"].as_str().unwrap().to_string();
    if let Err(error) = ProjectionStore::open_readonly(&root, scope, &endpoint) {
        panic!("{step}: relocated authority at {} does not open as a valid store: {error:?}\n{state}", root.display());
    }
    root
}

fn status_node(ws: &TestWorkspace, team: &str, agent: &str) -> (Value, TaResult) {
    let out = ta(ws, &["status", "--workspace", &ws_str(ws), "--team", team, "--json"]);
    let value: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
    let node = value["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["name"].as_str() == Some(agent)).cloned())
        .unwrap_or(Value::Null);
    (node, out)
}

/// Status of the contract seat must not report a lost or missing native authority.
fn assert_status_healthy(ws: &TestWorkspace, team: &str, step: &str) {
    let (node, out) = status_node(ws, team, NATIVE);
    let text = node.to_string();
    assert!(
        !node.is_null() && !text.contains("native_authority_lost") && !text.contains("native_authority_missing"),
        "{step}: status for {NATIVE} reports a broken native authority (or no node): {text}\n{}\n{}",
        transcript(&out),
        disk_state(ws, team)
    );
    assert_ne!(
        node["runtime_status"].as_str(),
        Some("running"),
        "{step}: the explicitly stopped contract seat must stay stopped: {text}"
    );
}

fn assert_no_authority_error(out: &TaResult, step: &str) {
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(
        out.exit_code == 0 && !all.contains("native_authority_lost") && !all.contains("no eligible nonpaused roles"),
        "{step}: expected success without a native authority failure\n{}",
        transcript(out)
    );
}

// ---------------------------------------------------------------- D1: copy-forward

/// Explicit `migrate` must move a live legacy temp authority into the workspace, keep its
/// content, and leave the team healthy after the OS purges /tmp.
#[test]
fn r1_migrate_copies_live_temp_authority_into_the_workspace() {
    let tag = "rbt-r1";
    let ws = team(tag);
    let temp = TempRoot::fresh();
    let legacy = write_legacy_binding(&ws, tag, &ws.path().canonicalize().unwrap(), 0, &temp.0);
    let (rel, bytes) = create_store(&legacy);
    let out = ta(&ws, &["migrate", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_eq!(out.exit_code, 0, "migrate must succeed on a live legacy authority\n{}", transcript(&out));
    let root = assert_authority_inside_workspace(&ws, tag, &temp.0, "after migrate");
    assert_eq!(
        fs::read(root.join(&rel)).ok(),
        Some(bytes),
        "native evidence {} was not carried to the relocated authority {}",
        rel.display(),
        root.display()
    );
    reboot(&ws, Some(&temp.0), Purge::Whole);
    assert_status_healthy(&ws, tag, "after reboot following migrate");
}

/// Any lifecycle-mutating command on the team (here: stopping its legacy worker) must copy the
/// live temp authority forward. In the field, legacy starts ran for 8 days and none moved it.
#[test]
fn r2_lifecycle_command_copies_live_temp_authority_before_the_reboot() {
    let tag = "rbt-r2";
    let ws = team(tag);
    let temp = TempRoot::fresh();
    let legacy = write_legacy_binding(&ws, tag, &ws.path().canonicalize().unwrap(), 0, &temp.0);
    let (rel, bytes) = create_store(&legacy);
    let out = ta(&ws, &["stop", WORKER, "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_eq!(out.exit_code, 0, "stopping the legacy worker must succeed\n{}", transcript(&out));
    let root = assert_authority_inside_workspace(&ws, tag, &temp.0, "after legacy stop");
    assert_eq!(fs::read(root.join(&rel)).ok(), Some(bytes), "native evidence {} was not carried", rel.display());
    reboot(&ws, Some(&temp.0), Purge::Whole);
    assert_status_healthy(&ws, tag, "after reboot following legacy stop");
}

/// Read-only doctor must warn, before any damage, that the authority lives in OS temp storage.
#[test]
fn r3_doctor_warns_that_authority_lives_in_os_temp_storage() {
    let tag = "rbt-r3";
    let ws = team(tag);
    let temp = TempRoot::fresh();
    let legacy = write_legacy_binding(&ws, tag, &ws.path().canonicalize().unwrap(), 0, &temp.0);
    create_store(&legacy);
    let out = ta(&ws, &["doctor", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(
        all.contains(temp.0.to_str().unwrap()),
        "doctor must name the temp authority root {} as a reboot/tmp_cleaner hazard\n{}",
        temp.0.display(),
        transcript(&out)
    );
}

// ---------------------------------------------------------------- D2: zero-eligible recovery

fn purged_team(tag: &str, purge: Purge) -> (TestWorkspace, TempRoot, LegacyBinding) {
    let ws = team(tag);
    let temp = TempRoot::fresh();
    let legacy = write_legacy_binding(&ws, tag, &ws.path().canonicalize().unwrap(), 0, &temp.0);
    create_store(&legacy);
    reboot(&ws, Some(&temp.0), purge);
    (ws, temp, legacy)
}

fn rehome_with_allow_fresh(tag: &str, purge: Purge) {
    let (ws, temp, legacy) = purged_team(tag, purge);
    let dry = ta(&ws, &["team", "restart", "--allow-fresh", "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&dry, "dry-run of --allow-fresh with zero eligible roles");
    let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&out, "--allow-fresh with zero eligible roles");
    assert_authority_inside_workspace(&ws, tag, &temp.0, "after --allow-fresh");
    let archived = fs::read_dir(&legacy.dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name() != "framework-binding.json" && e.path().is_file())
        .any(|e| fs::read_to_string(e.path()).unwrap_or_default().contains(temp.0.to_str().unwrap()));
    assert!(archived, "the original binding (naming {}) must be archived, not discarded\n{}", temp.0.display(), disk_state(&ws, tag));
    assert_status_healthy(&ws, tag, "after --allow-fresh");
    let start = ta(&ws, &["start", WORKER, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&start, "legacy start planning after re-home");
}

/// Field deadlock: the only contract seat is explicitly stopped, so recovery refused with
/// "no eligible nonpaused roles" before it re-homed the authority.
#[test]
fn r4_allow_fresh_rehomes_a_purged_authority_when_no_role_is_eligible() {
    rehome_with_allow_fresh("rbt-r4", Purge::Whole);
}

/// The tmp_cleaner shape (directories survive, files are gone) must recover identically.
#[test]
fn r5_allow_fresh_rehomes_a_tmp_cleaner_purged_authority() {
    rehome_with_allow_fresh("rbt-r5", Purge::RegularFiles);
}

// ---------------------------------------------------------------- D3: legacy seats unaffected

/// A dead native store must not block a pure legacy seat: planning and a real start succeed.
#[test]
fn r6_legacy_seat_starts_although_the_native_authority_was_purged() {
    let tag = "rbt-r6";
    let (ws, _temp, _legacy) = purged_team(tag, Purge::Whole);
    let dry = ta(&ws, &["start", WORKER, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&dry, "legacy start planning with a purged native authority");
    let out = ta(&ws, &["start", WORKER, "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    own_sockets(&ws);
    assert_no_authority_error(&out, "legacy start with a purged native authority");
    let (node, status) = status_node(&ws, tag, WORKER);
    assert_eq!(node["runtime_status"].as_str(), Some("running"), "legacy worker must be running: {node}\n{}", transcript(&status));
}

// ---------------------------------------------------------------- D4: device renumbering

/// macOS renumbered the data volume across the Oct 28 reboot (st_dev 0x1000011 -> 0x100000f):
/// a binding written on the earlier boot must still be found, and no second binding created.
#[test]
fn r7_binding_written_before_a_device_renumbering_is_still_found() {
    let tag = "rbt-r7";
    let ws = team(tag);
    let real = ws.path().canonicalize().unwrap();
    // Durable authority: the scope dir name itself is the only thing the drift breaks.
    let scope_probe = write_legacy_binding(&ws, tag, &real, 0x2, Path::new("/nonexistent"));
    fs::remove_dir_all(&scope_probe.dir).unwrap();
    let authority = scope_probe.dir.join("authority");
    let legacy = write_legacy_binding(&ws, tag, &real, 0x2, &authority);
    create_store(&legacy);
    reboot(&ws, None, Purge::Whole);
    assert_status_healthy(&ws, tag, "binding from an earlier boot (different st_dev)");
    let out = ta(&ws, &["stop", WORKER, "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    log("r7", transcript(&out));
    let found = bindings(&ws, tag);
    assert_eq!(found.len(), 1, "a device renumbering must not create a second binding\n{}", disk_state(&ws, tag));
    assert_status_healthy(&ws, tag, "after a lifecycle command on the renumbered device");
}

// ---------------------------------------------------------------- guards (green on baseline)

/// Consent stays explicit: without --allow-fresh a purged authority is refused with guidance.
#[test]
fn g1_purged_authority_without_allow_fresh_is_refused_with_guidance() {
    let tag = "rbt-g1";
    let (ws, _temp, _legacy) = purged_team(tag, Purge::Whole);
    let out = ta(&ws, &["team", "restart", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(out.exit_code != 0 && all.contains("--allow-fresh"), "expected a refusal naming --allow-fresh\n{}", transcript(&out));
}

/// A missing NON-temporary authority is never silently replaced.
#[test]
fn g2_missing_durable_authority_is_not_replaced() {
    let tag = "rbt-g2";
    let ws = team(tag);
    let real = ws.path().canonicalize().unwrap();
    let probe = write_legacy_binding(&ws, tag, &real, 0, Path::new("/nonexistent"));
    fs::remove_dir_all(&probe.dir).unwrap();
    let durable = probe.dir.join("authority");
    write_legacy_binding(&ws, tag, &real, 0, &durable);
    reboot(&ws, None, Purge::Whole);
    let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_ne!(out.exit_code, 0, "a missing durable authority must be refused\n{}", transcript(&out));
    assert!(!durable.exists(), "no replacement authority may be created at {}\n{}", durable.display(), disk_state(&ws, tag));
}

/// Contract-family seats keep failing closed while their authority is lost.
#[test]
fn g3_contract_seat_still_fails_closed_on_a_purged_authority() {
    let tag = "rbt-g3";
    let (ws, _temp, _legacy) = purged_team(tag, Purge::Whole);
    let out = ta(&ws, &["start", NATIVE, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(out.exit_code != 0 && all.contains("native_authority"), "expected a fail-closed native authority refusal\n{}", transcript(&out));
}

/// Harness validity: without any binding the same rebooted team starts its legacy worker.
#[test]
fn g4_control_rebooted_team_without_binding_starts_its_legacy_worker() {
    let tag = "rbt-g4";
    let ws = team(tag);
    reboot(&ws, None, Purge::Whole);
    let out = ta(&ws, &["start", WORKER, "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    own_sockets(&ws);
    assert_eq!(out.exit_code, 0, "control start failed: harness problem, not SUT\n{}", transcript(&out));
}

/// With a current-boot binding and a stale earlier-boot sibling, the current one is used.
#[test]
fn g5_current_boot_binding_wins_over_a_stale_sibling() {
    let tag = "rbt-g5";
    let ws = team(tag);
    let real = ws.path().canonicalize().unwrap();
    let stale_temp = TempRoot::fresh();
    write_legacy_binding(&ws, tag, &real, 0x2, &stale_temp.0);
    let probe = write_legacy_binding(&ws, tag, &real, 0, Path::new("/nonexistent"));
    fs::remove_file(probe.dir.join("framework-binding.json")).unwrap();
    let current = write_legacy_binding(&ws, tag, &real, 0, &probe.dir.join("authority"));
    create_store(&current);
    reboot(&ws, Some(&stale_temp.0), Purge::Whole);
    assert_status_healthy(&ws, tag, "current-boot binding beside a stale sibling");
}

/// A binding recorded for a different workspace path is never adopted.
#[test]
fn g6_binding_recorded_for_another_workspace_is_not_adopted() {
    let tag = "rbt-g6";
    let ws = team(tag);
    let other = Path::new("/private/tmp/ta-e2e-not-this-workspace");
    let temp = TempRoot::fresh();
    let foreign = write_legacy_binding(&ws, tag, other, 0, &temp.0);
    create_store(&foreign);
    let (node, out) = status_node(&ws, tag, NATIVE);
    assert!(
        node.to_string().contains("native_authority_missing"),
        "a binding for {} must not be adopted by {}: {node}\n{}",
        other.display(),
        ws.path().display(),
        transcript(&out)
    );
}

// ---------------------------------------------------------------- D1b: copy-forward re-points native seats

/// A real, controlled-terminal native seat (real Lifecycle, real tmux, real process), committed and
/// then torn down, so the store holds a stopped seat with full physical receipts.
mod native_fixture {
    use super::physical_support;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use team_agent_contract::{
        contract::{descriptor::{AuthMode, FreshSession}, hooks::PlanHook, plan::*, types::*},
        host::{clock::RealClock, command::{OutputLimits, RealCommandRunner}, materialize::ScopedMaterializer,
            process::{fingerprint_file, resolve_cwd}, tmux::{HostLimits, TmuxHost}, transport::TargetReceipt},
        orchestration::{lifecycle::*, physical::*, store::*},
    };

    fn limits() -> HostLimits {
        HostLimits { command_output: OutputLimits { stdout: 256 * 1024, stderr: 65536, stdin: 1024 * 1024 },
            max_script_bytes: 128 * 1024, max_capture_bytes: 256 * 1024, max_payload_bytes: 1024 * 1024,
            max_executable_bytes: 1024 * 1024 * 1024, poll_interval: Duration::from_millis(10), columns: 120, rows: 40 }
    }

    /// Closes the fixture pane if the test dies between startup and teardown.
    struct Owned { tmux: PathBuf, target: Option<TargetReceipt> }
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some(target) = self.target.take() {
                let clock = RealClock::new();
                if let Ok(mut host) = TmuxHost::restore_owned(target.clone(), self.tmux.clone(), limits(),
                    RealCommandRunner::default(), &clock, Duration::from_secs(3)) {
                    let _ = host.close_owned_pane(&target, &clock, Duration::from_secs(3));
                }
            }
        }
    }

    /// The seat's cwd and native directory both live under `runtime_root`, like 0.5.127-era Codex seats
    /// (`<temp root>/workspaces/<seat>` and `<temp root>/native-<hash>/`).
    pub fn stopped_seat(store: &mut ProjectionStore, runtime_root: &Path, cwd_dir: &Path, seat: &str, image_dir: &Path) -> InstanceIdentity {
        let tmux = super::tmux_path();
        let cwd = resolve_cwd(cwd_dir).unwrap();
        let identity = store.propose_identity(&SeatId::new(seat).unwrap()).unwrap();
        let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
        let candidate_sha256 = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(5)).unwrap();
        let (image, native_hash) = physical_support::native_terminal::copy_executable(image_dir);
        let native = physical_support::native(native_hash);
        let mut descriptor = physical_support::descriptor("single", &native);
        descriptor.identity.id = "pi_tui";
        descriptor.session.fresh = FreshSession::NotApplicable;
        let hooks = physical_support::native_terminal::hooks();
        let request = LaunchRequest { identity: identity.clone(), provider: "pi_tui".into(), native,
            operation: Operation::Fresh, mode: LaunchMode::FullWorker, auth: AuthMode::NativeSubscription,
            model: Some("single".into()), role_effort: None, team_effort: None, prompt: Some("reboot drill seat".into()),
            bypass: false, paths: LaunchPaths { candidate, cwd, runtime_root: runtime_root.into(),
                executable: image.clone(), launcher: physical_support::public_launcher(&image, native_hash) },
            channel: Channel::Tmux, input_profile: Some("physical-fixture".into()), evidence_kind: EvidenceKind::Native,
            preassigned_session: None, resume: None, fork: None };
        let resolved = resolve_launch(&descriptor, &hooks, &request, None).unwrap();
        let adapter = Adapter { descriptor: &descriptor, hooks: &hooks, catalog: None };
        let plan = physical_support::native_terminal::PLAN.plan(&resolved).unwrap();
        let operation = OperationId::new("r8-start").unwrap();
        let mut io = ScopedMaterializer::new(&descriptor, &resolved, &plan, operation.clone(), RealCommandRunner::default()).unwrap();
        let clock = RealClock::new();
        let mut physical = PhysicalRuntime::new(Registration { descriptor: &descriptor, hooks: &hooks, controls: &[],
            startup: None, session_panel: None }, PhysicalSettings { tmux: tmux.clone(), limits: limits(),
            action_budget: Duration::from_secs(10), freshness: Duration::from_secs(2), journal_bytes: 2 * 1024 * 1024 },
            operation.clone(), &clock).unwrap();
        physical.executing_candidate = Some(candidate_sha256);
        let mut owned = Owned { tmux, target: None };
        let started = Lifecycle { store: &mut *store, host: &mut physical, io: &mut io }.startup(&adapter, &request,
            Routing { pane: "unpublished".into(), binding_key: "unpublished-r8".into(), server_key: "fixture-server".into() }, operation).unwrap();
        assert_eq!(started.outcome, Outcome::Committed, "fixture startup: {started:?}");
        owned.target = store.assert_current(&identity).unwrap().physical.clone();
        let stopped = Lifecycle { store: &mut *store, host: &mut physical, io: &mut io }
            .teardown(&identity, OperationId::new("r8-stop").unwrap()).unwrap();
        assert_eq!(stopped.outcome, Outcome::Committed, "fixture teardown: {stopped:?}");
        owned.target = None;
        identity
    }
}

// ---------------------------------------------------------------- D2b: skipped native seats stay startable

/// `<root>/cursor-fake/<VERSION>/{cursor-agent,node,index.js}`: the vendor layout Cursor discovery
/// accepts. Only `--version` and `--list-models` answer; a TUI launch exits at once. Returns the PATH dir.
fn fake_cursor(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    const VERSION: &str = "2026.10.22-27dcf5f";
    let dir = root.join("cursor-fake").join(VERSION);
    fs::create_dir_all(&dir).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n--version) echo {VERSION}; exit 0 ;;\n--list-models) printf 'Available models\\n\\ngrok-4.7-high - Grok 4.7 High\\n\\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\\n'; exit 0 ;;\nesac\nexit 7\n"
    );
    fs::write(dir.join("cursor-agent"), script).unwrap();
    fs::set_permissions(dir.join("cursor-agent"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy("/bin/sleep", dir.join("node")).unwrap();
    fs::set_permissions(dir.join("node"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.join("index.js"), b"// fake entry\n").unwrap();
    dir
}
