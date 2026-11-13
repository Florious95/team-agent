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

/// An external durable authority root, outside both the OS temp area and the workspace.
struct ExternalRoot(PathBuf);
impl ExternalRoot {
    fn fresh() -> Self {
        let tmp = Path::new("/tmp").canonicalize().unwrap();
        let seed = format!("{}-{}-{:?}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed), std::time::SystemTime::now());
        let path = tmp.join(format!("rbt-external-{}", &hex(&Sha256::digest(seed.as_bytes()))[..24]));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for ExternalRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// g2' (revised for ISSUE-8, owner-authorized 2026-11-13): a missing durable authority whose
/// root lies outside both the OS temp area and this workspace is never replaced, even with
/// --allow-fresh. A deleted in-workspace authority is recoverable instead (r12).
#[test]
fn g2_missing_durable_authority_is_not_replaced() {
    let tag = "rbt-g2";
    let ws = team(tag);
    let real = ws.path().canonicalize().unwrap();
    let external = ExternalRoot::fresh();
    let durable = external.0.join("authority");
    assert!(!durable.starts_with(&real), "fixture: the external root must lie outside the workspace");
    write_legacy_binding(&ws, tag, &real, 0, &durable);
    reboot(&ws, None, Purge::Whole);
    let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_ne!(out.exit_code, 0, "a missing external durable authority must be refused\n{}", transcript(&out));
    assert!(!durable.exists(), "no replacement authority may be created at {}\n{}", durable.display(), disk_state(&ws, tag));
    let found = bindings(&ws, tag);
    assert!(
        found.len() == 1 && found[0].1["transport_root"].as_str() == durable.to_str(),
        "the binding must still name the missing external root (no re-home)\n{}",
        disk_state(&ws, tag)
    );
}

/// ISSUE-8: an authority that lived inside this workspace's own scope directory and was deleted
/// is re-homed by explicit `team restart --allow-fresh`, archiving the old binding first.
#[test]
fn r12_allow_fresh_rehomes_a_deleted_in_workspace_authority() {
    let tag = "rbt-r12";
    let ws = team(tag);
    let real = ws.path().canonicalize().unwrap();
    let probe = write_legacy_binding(&ws, tag, &real, 0, Path::new("/nonexistent"));
    fs::remove_dir_all(&probe.dir).unwrap();
    let durable = probe.dir.join("authority");
    let legacy = write_legacy_binding(&ws, tag, &real, 0, &durable);
    create_store(&legacy);
    reboot(&ws, None, Purge::Whole);
    fs::remove_dir_all(&durable).unwrap();
    log("r12", format!("deleted in-workspace authority {}", durable.display()));
    let dry = ta(&ws, &["team", "restart", "--allow-fresh", "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&dry, "dry-run of --allow-fresh for a deleted in-workspace authority");
    let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&out, "--allow-fresh for a deleted in-workspace authority");
    assert_authority_inside_workspace(&ws, tag, &durable, "after --allow-fresh re-homed a deleted in-workspace authority");
    let archived = fs::read_dir(&legacy.dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name() != "framework-binding.json" && e.path().is_file())
        .any(|e| fs::read_to_string(e.path()).unwrap_or_default().contains(durable.to_str().unwrap()));
    assert!(archived, "the original binding (naming {}) must be archived, not discarded\n{}", durable.display(), disk_state(&ws, tag));
    assert_status_healthy(&ws, tag, "after --allow-fresh re-homed a deleted in-workspace authority");
    let start = ta(&ws, &["start", WORKER, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&start, "legacy start planning after the in-workspace re-home");
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

// ---------------------------------------------------------------- D5: adding a seat after the purge
//
// Field incident 2026-11-13: a workspace whose team was created on 2026-11-02 kept its native
// authority in `/private/tmp/ta-<24 hex>`. A host reboot on 2026-11-10 purged it. The user then
// declared a NEW legacy seat and ran
// `team-agent start fable --role-file .team/current/agents/fable.md --workspace . --team current`.
// The command was refused with `agent start requirement unmet: native_authority_lost: …`. r6 only
// covers a legacy seat that already has a state row.

const NEW_SEAT: &str = "newbie";

/// Declare a brand-new legacy seat (no state row) in the team's role directory.
fn declare_new_legacy_seat(ws: &TestWorkspace) -> PathBuf {
    let path = ws.path().join("agents").join(format!("{NEW_SEAT}.md"));
    fs::write(
        &path,
        format!("---\nname: {NEW_SEAT}\nrole: Fake worker {NEW_SEAT}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker {NEW_SEAT}.\n"),
    )
    .unwrap();
    path
}

fn assert_no_state_row(ws: &TestWorkspace, team: &str, agent: &str) {
    let state = ws.read_state();
    for row in [&state["agents"][agent], &state["teams"][team]["agents"][agent]] {
        assert!(row.is_null(), "fixture invariant: {agent} must have no state row before its first start: {row}");
    }
}

fn start_new_legacy_seat_after_purge(tag: &str, purge: Purge) {
    let (ws, temp, _legacy) = purged_team(tag, purge);
    declare_new_legacy_seat(&ws);
    assert_no_state_row(&ws, tag, NEW_SEAT);
    let before = bindings(&ws, tag);
    let role = format!("agents/{NEW_SEAT}.md");
    let dry = ta(&ws, &["start", NEW_SEAT, "--role-file", &role, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert_no_authority_error(&dry, &format!("{purge:?}: dry-run of adding a new legacy seat after the native authority was purged"));
    let out = ta(&ws, &["start", NEW_SEAT, "--role-file", &role, "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    own_sockets(&ws);
    assert_no_authority_error(&out, &format!("{purge:?}: adding a new legacy seat after the native authority was purged"));
    let (node, status) = status_node(&ws, tag, NEW_SEAT);
    assert_eq!(node["runtime_status"].as_str(), Some("running"), "{purge:?}: the new legacy seat must be running: {node}\n{}", transcript(&status));
    // Adding a legacy seat neither recreates nor re-homes the lost native authority.
    assert_eq!(bindings(&ws, tag), before, "{purge:?}: a legacy start must leave the framework binding untouched\n{}", disk_state(&ws, tag));
    match purge {
        Purge::Whole => assert!(!temp.0.exists(), "{purge:?}: the purged authority root {} must not be recreated", temp.0.display()),
        Purge::RegularFiles => assert!(
            !temp.0.join("contract.json").exists(),
            "{purge:?}: no contract manifest may be recreated under {}",
            temp.0.display()
        ),
    }
}

/// The field incident: a NEW legacy seat, declared with `--role-file`, starts (dry-run and real)
/// although the team's temporary native authority was purged.
#[test]
fn r10_new_legacy_seat_added_with_role_file_starts_although_the_native_authority_was_purged() {
    start_new_legacy_seat_after_purge("rbt-r10w", Purge::Whole);
    start_new_legacy_seat_after_purge("rbt-r10f", Purge::RegularFiles);
}

/// A per-seat native role snapshot is written to the workspace scope root before any native carrier
/// spawns, so it is durable evidence that this seat ID had native authority. A new seat with such a
/// snapshot must keep failing closed while that authority is lost, whatever provider it now declares.
#[test]
fn g7_new_seat_with_a_native_role_snapshot_on_disk_stays_fail_closed() {
    for (tag, snapshot) in [
        ("rbt-g7i", format!("role-{NEW_SEAT}-instance-{}.json", "7a".repeat(32))),
        ("rbt-g7g", format!("role-{NEW_SEAT}.json")),
    ] {
        let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
        declare_new_legacy_seat(&ws);
        assert_no_state_row(&ws, tag, NEW_SEAT);
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(legacy.dir.join(&snapshot)).unwrap();
        file.write_all(format!("{{\"id\":\"{NEW_SEAT}\"}}").as_bytes()).unwrap();
        file.sync_all().unwrap();
        let role = format!("agents/{NEW_SEAT}.md");
        let out = ta(&ws, &["start", NEW_SEAT, "--role-file", &role, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
        let all = format!("{}{}", out.stdout, out.stderr);
        assert!(
            out.exit_code != 0 && all.contains("native_authority"),
            "{snapshot}: a seat with a native role snapshot must fail closed while its authority is lost\n{}",
            transcript(&out)
        );
    }
}

/// Guidance for a purged TEMPORARY authority must name the executable consent path
/// (`team restart --allow-fresh`). `migrate` cannot recreate a purged authority, so neither the
/// start refusal nor doctor may recommend it.
#[test]
fn r11_lost_temporary_authority_guidance_names_the_executable_remedy() {
    let tag = "rbt-r11";
    let (ws, _temp, _legacy) = purged_team(tag, Purge::Whole);
    let start = ta(&ws, &["start", NATIVE, "--dry-run", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let start_text = format!("{}{}", start.stdout, start.stderr);
    assert!(start.exit_code != 0 && start_text.contains("native_authority"), "the contract seat must still fail closed\n{}", transcript(&start));
    let doctor = ta(&ws, &["doctor", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let doctor_text = format!("{}{}", doctor.stdout, doctor.stderr);
    // Evaluate both surfaces before asserting, so one run reports every deficit.
    let mut deficits = Vec::new();
    if !start_text.contains("team restart --allow-fresh") || start_text.contains("team-agent migrate") {
        deficits.push(format!("start refusal must name `team restart --allow-fresh`, not migrate\n{}", transcript(&start)));
    }
    if !doctor_text.contains("team restart --allow-fresh") || doctor_text.contains("team-agent migrate") || doctor_text.contains("\"migrate\"") {
        deficits.push(format!("doctor must name `team restart --allow-fresh` and must not recommend migrate\n{}", transcript(&doctor)));
    }
    assert!(deficits.is_empty(), "guidance on a purged temporary authority:\n{}", deficits.join("\n\n"));
}

// ---------------------------------------------------------------- D4: pane ids reused after a reboot
//
// Field (C01, 2026-11-13): after a reboot the workspace tmux server is new and pane ids restart
// at %0. A legacy seat started first takes the pane id that the pre-reboot native carrier had
// recorded, and `team restart --allow-fresh` mistakes the live legacy pane for the old carrier.
// `team stop` on a lost-authority native seat whose carrier is gone reports "native seats not
// stopped". Native rows here carry the field shape of `astra` (frozen role snapshot included).

const NATIVE_INSTANCE: &str = "instance-before-reboot";

/// Overwrite the contract row of `NATIVE` (top-level and team-scoped) with `row`.
fn set_native_row(ws: &TestWorkspace, tag: &str, row: &Value) {
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
}

/// The pre-reboot native seat: committed instance, frozen role snapshot, saved carrier location.
fn native_row(ws: &TestWorkspace, tag: &str, legacy: &LegacyBinding, endpoint: &str, pane: &str, running: bool) -> Value {
    let body = format!("{NATIVE} frozen prompt");
    let snapshot = json!({
        "id": NATIVE, "provider": "codex_tui", "source": ws.path().join("agents").join(format!("{NATIVE}.md")),
        "role": NATIVE, "model": "first-model", "effort": null, "bypass": false, "paused": false, "body": body,
        "prompt": {"text": body, "sha256": Sha256::digest(body.as_bytes()).to_vec(), "sources": []},
    });
    let path = legacy.dir.join(format!("role-{NATIVE}-{NATIVE_INSTANCE}.json"));
    if !path.exists() {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
        file.write_all(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        file.sync_all().unwrap();
    }
    json!({
        "agent_id": NATIVE, "provider": "codex_tui", "contract_provider": "codex_tui", "runtime_family": "contract",
        "status": if running { "running" } else { "stopped" }, "explicit_stop": !running, "paused": false,
        "contract_scope": legacy.scope, "contract_instance": NATIVE_INSTANCE, "generation": 1,
        "native_endpoint": endpoint, "native_pane": pane, "tmux_endpoint": endpoint, "pane_id": pane,
        "model": "first-model", "role": NATIVE, "owner_team_id": tag, "team_key": tag,
    })
}

fn tmux_on(socket: &str, args: &[&str]) -> String {
    let out = std::process::Command::new("tmux").arg("-S").arg(socket).args(args).output().unwrap();
    assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn pane_alive(socket: &str, pane: &str) -> bool {
    let out = std::process::Command::new("tmux").arg("-S").arg(socket).args(["display-message", "-p", "-t", pane, "#{pane_dead}"]).output().unwrap();
    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "0"
}

/// After the reboot, the legacy worker starts first on the new server. Returns its row.
fn legacy_started_first(ws: &TestWorkspace, tag: &str) -> Value {
    let out = ta(ws, &["start", WORKER, "--workspace", &ws_str(ws), "--team", tag, "--json"]);
    own_sockets(ws);
    assert_eq!(out.exit_code, 0, "fixture: the legacy worker must start after the reboot\n{}", transcript(&out));
    let state = ws.read_state();
    let row = state["teams"][tag]["agents"][WORKER].clone();
    let row = if row.is_object() { row } else { state["agents"][WORKER].clone() };
    assert!(row["pane_id"].is_string() && row["pane_pid"].is_u64(), "fixture: the legacy row must record its live pane: {row}");
    let socket = state["tmux_socket"].as_str().unwrap_or_default().to_string();
    assert!(pane_alive(&socket, row["pane_id"].as_str().unwrap()), "fixture: the legacy pane must be live");
    row
}

fn recover_dry_run(ws: &TestWorkspace, tag: &str) -> TaResult {
    ta(ws, &["team", "restart", "--allow-fresh", "--dry-run", "--workspace", &ws_str(ws), "--team", tag, "--json"])
}

/// Control: the saved native pane id does not exist on the new server; recovery planning is admitted.
#[test]
fn g8_control_recovery_admits_a_saved_native_pane_absent_from_the_new_server() {
    let tag = "rbt-g8";
    let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
    legacy_started_first(&ws, tag);
    let socket = ws.read_state()["tmux_socket"].as_str().unwrap().to_string();
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, "%999", false));
    assert_no_authority_error(&recover_dry_run(&ws, tag), "recovery planning with an absent saved native pane");
}

/// D4: the saved native pane id now belongs to the verified live legacy worker. The old native
/// carrier is absent; recovery is admitted and the legacy pane is untouched.
#[test]
fn r13_allow_fresh_recognizes_a_reused_pane_owned_by_a_verified_live_legacy_seat() {
    let tag = "rbt-r13";
    let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
    let worker = legacy_started_first(&ws, tag);
    let socket = ws.read_state()["tmux_socket"].as_str().unwrap().to_string();
    let pane = worker["pane_id"].as_str().unwrap().to_string();
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, &pane, false));
    let dry = recover_dry_run(&ws, tag);
    assert_no_authority_error(&dry, &format!("recovery planning when the saved native pane {pane} is the live legacy worker's pane"));
    assert!(pane_alive(&socket, &pane), "recovery planning must not touch the legacy pane {pane}");
}

/// Guards: a reused pane id whose live owner is not positively verified as a legacy seat, or that
/// the native row itself claims by live process identity, still forbids recovery (fail closed).
#[test]
fn g9_reused_pane_with_an_unverified_or_native_owner_still_blocks_recovery() {
    for case in ["unknown", "unverified", "native"] {
        let tag = format!("rbt-g9{}", &case[..1]);
        let (ws, _temp, legacy) = purged_team(&tag, Purge::Whole);
        let worker = legacy_started_first(&ws, &tag);
        let state = ws.read_state();
        let socket = state["tmux_socket"].as_str().unwrap().to_string();
        let worker_pane = worker["pane_id"].as_str().unwrap().to_string();
        let (pane, row) = match case {
            // A live pane in another session that no legacy row claims.
            "unknown" => {
                let pane = tmux_on(&socket, &["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "intruder", "sleep 600"]);
                (pane.clone(), native_row(&ws, &tag, &legacy, &socket, &pane, false))
            }
            // The legacy row claims the pane, but its recorded process identity does not match.
            "unverified" => {
                let mut forged = worker.clone();
                forged["pane_pid"] = json!(1);
                ws.mutate_state(|state| {
                    for pointer in [format!("/teams/{tag}/agents/{WORKER}"), format!("/agents/{WORKER}")] {
                        if let Some(row) = state.pointer_mut(&pointer).filter(|row| row.is_object()) {
                            *row = forged.clone();
                        }
                    }
                });
                (worker_pane.clone(), native_row(&ws, &tag, &legacy, &socket, &worker_pane, false))
            }
            // The native row itself records the live process in that pane.
            _ => {
                let mut row = native_row(&ws, &tag, &legacy, &socket, &worker_pane, false);
                for key in ["pane_pid", "pane_birth_identity", "pane_birth", "spawned_at"] {
                    if !worker[key].is_null() {
                        row[key] = worker[key].clone();
                    }
                }
                (worker_pane.clone(), row)
            }
        };
        set_native_row(&ws, &tag, &row);
        let before = bindings(&ws, &tag);
        let dry = recover_dry_run(&ws, &tag);
        let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", &tag, "--json"]);
        assert!(dry.exit_code != 0 && out.exit_code != 0, "{case}: a live pane {pane} without a verified legacy owner must forbid recovery\n{}\n{}", transcript(&dry), transcript(&out));
        assert_eq!(bindings(&ws, &tag), before, "{case}: refused recovery must not re-home the authority\n{}", disk_state(&ws, &tag));
        assert!(pane_alive(&socket, &pane), "{case}: refused recovery must not touch pane {pane}");
    }
}

/// Team stop of a lost-authority native seat whose saved carrier is provably absent.
fn stop_lost_authority_native(tag: &str, seat_scoped: bool) {
    let (ws, temp, legacy) = purged_team(tag, Purge::Whole);
    let state = ws.read_state();
    // The pre-reboot server was killed: its endpoint is gone or a stale socket with no server.
    let socket = state["tmux_socket"].as_str().unwrap_or_default().to_string();
    assert!(Path::new(&socket).is_absolute(), "fixture: the saved endpoint must be absolute: {socket}");
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, "%1", true));
    let before = bindings(&ws, tag);
    let mut args = vec!["stop"];
    if seat_scoped {
        args.push(NATIVE);
    }
    args.extend_from_slice(&["--workspace", ws.path().to_str().unwrap(), "--team", tag, "--json"]);
    let out = ta(&ws, &args);
    let text = format!("{}{}", out.stdout, out.stderr);
    assert!(out.exit_code == 0 && !text.contains("native seats not stopped"), "{args:?}: a lost-authority native seat with an absent carrier must stop cleanly\n{}", transcript(&out));
    let state = ws.read_state();
    for row in [&state["agents"][NATIVE], &state["teams"][tag]["agents"][NATIVE]] {
        assert_eq!(row["status"], "stopped", "{args:?}: the native row must be marked stopped: {row}");
    }
    let (node, status) = status_node(&ws, tag, NATIVE);
    assert_eq!(node["runtime_status"].as_str(), Some("stopped"), "{args:?}: status must report stopped: {node}\n{}", transcript(&status));
    assert_eq!(bindings(&ws, tag), before, "{args:?}: a metadata-only stop must not recreate or re-home the authority\n{}", disk_state(&ws, tag));
    assert!(!temp.0.exists(), "{args:?}: the purged authority root must not be recreated");
}

#[test]
fn r14_stop_marks_a_lost_authority_native_seat_stopped_when_its_carrier_is_absent() {
    stop_lost_authority_native("rbt-r14t", false);
    stop_lost_authority_native("rbt-r14s", true);
}

/// Guard: a lost-authority native seat whose saved pane is live with an unknown owner is never
/// reported stopped, and its pane is never touched.
#[test]
fn g10_lost_authority_native_stop_with_a_live_saved_pane_is_not_claimed() {
    let tag = "rbt-g10";
    let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
    let carrier = ws.tmux_root.path().join(format!("ta-{tag}-carrier"));
    let socket = carrier.to_str().unwrap().to_string();
    let pane = tmux_on(&socket, &["-f", "/dev/null", "new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "carrier", "sleep 600"]);
    ws.register_owned_tmux_socket(&carrier);
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, &pane, true));
    let out = ta(&ws, &["stop", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    let state = ws.read_state();
    let row = &state["teams"][tag]["agents"][NATIVE];
    assert!(out.exit_code != 0 || row["status"] != "stopped", "a live saved pane must not be reported stopped: {row}\n{}", transcript(&out));
    assert!(pane_alive(&socket, &pane), "a lost-authority stop must never touch the live pane {pane}");
}

/// Guard (old worker session fence): a session whose only extra pane is retained dead evidence
/// does not block recovery; the dead pane is left in place.
#[test]
fn g11_old_worker_session_with_only_dead_extra_panes_admits_recovery() {
    let tag = "rbt-g11";
    let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
    legacy_started_first(&ws, tag);
    let state = ws.read_state();
    let socket = state["tmux_socket"].as_str().unwrap().to_string();
    let session = state["session_name"].as_str().unwrap().to_string();
    let pane = tmux_on(&socket, &["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &format!("{session}:"), "sleep 600"]);
    tmux_on(&socket, &["set-option", "-w", "-t", &pane, "remain-on-exit", "on"]);
    let pid = tmux_on(&socket, &["display-message", "-p", "-t", &pane, "#{pane_pid}"]);
    assert!(std::process::Command::new("kill").args(["-9", &pid]).status().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while tmux_on(&socket, &["display-message", "-p", "-t", &pane, "#{pane_dead}"]) != "1" {
        assert!(std::time::Instant::now() < deadline, "fixture: pane {pane} never became dead");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, "%999", false));
    assert_no_authority_error(&recover_dry_run(&ws, tag), "recovery planning with a retained dead pane in the old worker session");
    assert_eq!(tmux_on(&socket, &["display-message", "-p", "-t", &pane, "#{pane_dead}"]), "1", "the retained dead pane must stay");
}

/// Guard (old worker session fence): an unrecognized live pane in the old worker session forbids
/// recovery; nothing is re-homed and the pane is untouched.
#[test]
fn g12_old_worker_session_with_an_unrecognized_live_pane_refuses_recovery() {
    let tag = "rbt-g12";
    let (ws, _temp, legacy) = purged_team(tag, Purge::Whole);
    legacy_started_first(&ws, tag);
    let state = ws.read_state();
    let socket = state["tmux_socket"].as_str().unwrap().to_string();
    let session = state["session_name"].as_str().unwrap().to_string();
    let pane = tmux_on(&socket, &["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &format!("{session}:"), "sleep 600"]);
    set_native_row(&ws, tag, &native_row(&ws, tag, &legacy, &socket, "%999", false));
    let before = bindings(&ws, tag);
    let dry = recover_dry_run(&ws, tag);
    let out = ta(&ws, &["team", "restart", "--allow-fresh", "--workspace", &ws_str(&ws), "--team", tag, "--json"]);
    assert!(dry.exit_code != 0 && out.exit_code != 0, "an unrecognized live pane {pane} in the old worker session must forbid recovery\n{}\n{}", transcript(&dry), transcript(&out));
    assert_eq!(bindings(&ws, tag), before, "refused recovery must not re-home the authority\n{}", disk_state(&ws, tag));
    assert!(pane_alive(&socket, &pane), "refused recovery must not touch pane {pane}");
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
