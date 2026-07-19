//! Car-B TARGET-INVARIANT contract v2 (verifier-frozen, r8 BLOCKING-4
//! remediation): the zombie-seat recovery invariants, re-authored to close the
//! false-green holes r8 found in the v1 contract (346ac9d7):
//!  - every "must succeed" asserts exit==0 AND ok==true (not just "no panic");
//!  - the death projection pins EVERY field to its exact value
//!    (worker_state==DEAD ∧ stale==true ∧ stale_reason==pane_dead), never a
//!    lone `status != running`;
//!  - stop asserts stopped==true (not merely ok);
//!  - the NEW `add-agent --force` surface is exercised after a real pane
//!    death, with a positive same-team-peer isolation assertion AND a negative
//!    cross-team same-name peer assertion (force must not touch a same-named
//!    seat in another team).
//!
//! Case source: `.team/artifacts/pipeline-runs/zombie-seat-unrecoverable/case.md`.
//! All fixtures canonical (real quick-start / real kill-pane / real CLI /
//! fake provider); the only fault is a real pane kill (MUST-15-listed).

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::PathBuf;
use std::process::Output;

use serde_json::{json, Value};
use serial_test::serial;

struct SeatCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    team: String,
    socket: PathBuf,
}

impl SeatCase {
    fn start(tag: &str, team: &str, workers: &[&str]) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::create_dir_all(workspace.join("masters")).expect("create masters dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!("---\nname: {team}\nobjective: zombie seat recovery contract v2.\nprovider: fake\n---\n"),
        )
        .expect("write TEAM.md");
        for worker in workers {
            std::fs::write(
                workspace.join("agents").join(format!("{worker}.md")),
                role_doc(worker),
            )
            .expect("write worker role doc");
        }
        let mut case = Self {
            env,
            workspace,
            team: team.to_string(),
            socket: PathBuf::new(),
        };
        let (ok, value) = case.cli_json(&[
            "quick-start",
            "--workspace",
            case.workspace_str(),
            "--team-id",
            team,
            "--yes",
            "--no-display",
            "--json",
        ]);
        // quick-start may exit nonzero for leader_receiver_unbound while still
        // spawning workers — assert the physical precondition, not ok here.
        assert!(
            value
                .get("worker_readiness")
                .and_then(|node| node.get("all_workers_spawned"))
                .and_then(Value::as_bool)
                == Some(true),
            "fixture: quick-start must spawn workers (ok={ok}); value={value}"
        );
        let state_raw = std::fs::read_to_string(
            case.workspace
                .join(".team")
                .join("runtime")
                .join("state.json"),
        )
        .expect("read state.json");
        let state: Value = serde_json::from_str(&state_raw).expect("parse state.json");
        case.socket = PathBuf::from(
            state
                .get("tmux_socket")
                .and_then(Value::as_str)
                .expect("state tmux_socket"),
        );
        case
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    /// Returns (exit-0 AND ok==true, parsed JSON). Both halves are checked by
    /// callers — an operation that "should succeed" must satisfy the bool.
    fn cli_json(&self, args: &[&str]) -> (bool, Value) {
        let output = self.run_cli(args);
        let value = json_stdout(&output, "cli");
        let ok = output.status.code() == Some(0)
            && value.get("ok").and_then(Value::as_bool) == Some(true);
        (ok, value)
    }

    fn pane_live(&self, window: &str) -> Option<String> {
        let output = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "list-panes",
                "-a",
                "-F",
                "#{window_name}__TA_FIELD__#{pane_id}__TA_FIELD__#{pane_dead}",
            ])
            .output()
            .expect("tmux list-panes");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let mut cols = line.split("__TA_FIELD__");
                let name = cols.next()?;
                let pane = cols.next()?.to_string();
                let dead = cols.next()?.trim();
                (name == window && dead == "0").then_some(pane)
            })
    }

    fn kill_pane(&self, window: &str) {
        let pane = self.pane_live(window).expect("live pane to kill");
        let _ = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "kill-pane",
                "-t",
                pane.as_str(),
            ])
            .output();
        std::thread::sleep(std::time::Duration::from_secs(3));
    }

    fn status_agent(&self, agent: &str) -> Option<Value> {
        let (_, value) = self.cli_json(&[
            "status",
            "--workspace",
            self.workspace_str(),
            "--team",
            &self.team,
            "--json",
        ]);
        value
            .get("agents")
            .and_then(|agents| agents.get(agent))
            .cloned()
    }

    /// Quick-start a SECOND team in the SAME workspace (shares the tmux
    /// endpoint). Used to build the shared-surface fixture Z5 needs.
    fn add_second_team_same_workspace(&self, team: &str, workers: &[&str]) {
        // Overwrite TEAM.md / agents for the second team's compile, then
        // quick-start it; the first team's runtime state is already persisted.
        std::fs::write(
            self.workspace.join("TEAM.md"),
            format!(
                "---\nname: {team}\nobjective: shared-endpoint second team.\nprovider: fake\n---\n"
            ),
        )
        .expect("write second TEAM.md");
        for worker in workers {
            std::fs::write(
                self.workspace.join("agents").join(format!("{worker}.md")),
                role_doc(worker),
            )
            .expect("write second team role doc");
        }
        let (_, value) = self.cli_json(&[
            "quick-start",
            "--workspace",
            self.workspace_str(),
            "--team-id",
            team,
            "--yes",
            "--no-display",
            "--json",
        ]);
        assert!(
            value
                .get("worker_readiness")
                .and_then(|node| node.get("all_workers_spawned"))
                .and_then(Value::as_bool)
                == Some(true),
            "fixture: second team {team} must spawn; value={value}"
        );
    }

    /// Pane id of a live window inside a specific tmux session on the shared
    /// socket (disambiguates same-named windows across teams).
    fn pane_for_window_in_session(&self, session: &str, window: &str) -> Option<String> {
        let output = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "list-panes",
                "-a",
                "-F",
                "#{session_name}__TA_FIELD__#{window_name}__TA_FIELD__#{pane_id}__TA_FIELD__#{pane_dead}",
            ])
            .output()
            .expect("tmux list-panes");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let mut cols = line.split("__TA_FIELD__");
                let s = cols.next()?;
                let w = cols.next()?;
                let pane = cols.next()?.to_string();
                let dead = cols.next()?.trim();
                (s == session && w == window && dead == "0").then_some(pane)
            })
    }

    fn shutdown_team(&self, team: &str) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--team",
            team,
            "--yes",
            "--json",
        ]);
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--yes",
            "--json",
        ]);
        let _ = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "kill-server",
            ])
            .output();
    }
}

fn role_doc(agent: &str) -> String {
    format!(
        "---\nname: {agent}\nrole: {agent}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{agent}.\n"
    )
}

fn json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: expected JSON stdout; code={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Z1 — idempotent exit + recreatable seat, with exit/ok pinned on every step.
#[test]
#[serial(env)]
fn z1_dead_seat_force_remove_is_idempotent_with_exit_ok_pinned() {
    let case = SeatCase::start("zsr2-z1", "zsr", &["w1", "w2"]);
    case.kill_pane("w1");

    let remove_args = [
        "remove-agent",
        "w1",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--from-spec",
        "--force",
        "--confirm",
        "--json",
    ];
    let (first_ok, first) = case.cli_json(&remove_args);
    assert!(
        first_ok,
        "Z1: first force-remove must exit 0 AND ok=true: {first}"
    );
    let (second_ok, second) = case.cli_json(&remove_args);
    assert!(
        second_ok,
        "Z1: second force-remove on an already-absent seat must ALSO exit 0 AND ok=true \
         (idempotent — no `unknown worker` dead end): {second}"
    );
    assert!(
        case.workspace.join("agents").join("w1.md").exists(),
        "Z1: the role doc must survive removal"
    );
    let (add_ok, add) = case.cli_json(&[
        "add-agent",
        "w1",
        "--role-file",
        case.workspace
            .join("agents")
            .join("w1.md")
            .to_str()
            .expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(add_ok, "Z1: re-add must exit 0 AND ok=true: {add}");
    assert!(
        case.pane_live("w1").is_some(),
        "Z1: recreated seat must be physically live"
    );
    case.shutdown();
}

/// Z2 — death projection pins EVERY field (r8: not a lone status != running).
#[test]
#[serial(env)]
fn z2_dead_pane_projection_pins_all_death_fields() {
    let case = SeatCase::start("zsr2-z2", "zsr", &["w1", "w2"]);
    case.kill_pane("w1");
    let agent = case
        .status_agent("w1")
        .expect("w1 present in status projection");
    assert_eq!(
        agent.get("worker_state").and_then(Value::as_str),
        Some("DEAD"),
        "Z2: a dead pane must project worker_state=DEAD (missing/other = false green); agent={agent}"
    );
    assert_eq!(
        agent.get("stale"),
        Some(&json!(true)),
        "Z2: death must set stale=true; agent={agent}"
    );
    assert_eq!(
        agent.get("stale_reason").and_then(Value::as_str),
        Some("pane_dead"),
        "Z2: death must set stale_reason=pane_dead; agent={agent}"
    );
    assert_eq!(
        agent.get("status").and_then(Value::as_str),
        Some("stopped"),
        "Z2: a dead pane must project the exact status=stopped (not merely non-running); agent={agent}"
    );
    case.shutdown();
}

/// Z3 — deletion scope: outside role files survive every remove form.
#[test]
#[serial(env)]
fn z3_role_masters_outside_team_dir_survive_every_remove_form() {
    let case = SeatCase::start("zsr2-z3", "zsr", &["w1", "w2"]);

    let master_a = case.workspace.join("masters").join("w3.md");
    std::fs::write(&master_a, role_doc("w3")).expect("write master a");
    let (add_a, add_a_v) = case.cli_json(&[
        "add-agent",
        "w3",
        "--role-file",
        master_a.to_str().expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(add_a, "Z3a fixture add w3 must ok: {add_a_v}");
    let (rm_a, rm_a_v) = case.cli_json(&[
        "remove-agent",
        "w3",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--force",
        "--confirm",
        "--json",
    ]);
    assert!(rm_a, "Z3a: remove (no --from-spec) must ok: {rm_a_v}");
    assert!(
        master_a.exists(),
        "Z3a: outside role file must survive remove without --from-spec"
    );

    let master_b = case.workspace.join("masters").join("w5.md");
    std::fs::write(&master_b, role_doc("w5")).expect("write master b");
    let (add_b, add_b_v) = case.cli_json(&[
        "add-agent",
        "w5",
        "--role-file",
        master_b.to_str().expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(add_b, "Z3b fixture add w5 must ok: {add_b_v}");
    let (rm_b, rm_b_v) = case.cli_json(&[
        "remove-agent",
        "w5",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--from-spec",
        "--force",
        "--confirm",
        "--json",
    ]);
    assert!(rm_b, "Z3b: remove --from-spec must ok: {rm_b_v}");
    assert!(
        master_b.exists(),
        "Z3b: outside role file must survive remove --from-spec"
    );

    let master_c = case.workspace.join("masters").join("w6-master.md");
    std::fs::write(&master_c, role_doc("w6")).expect("write master c");
    let link = case.workspace.join("agents").join("w6.md");
    std::os::unix::fs::symlink(&master_c, &link).expect("symlink");
    let (add_c, add_c_v) = case.cli_json(&[
        "add-agent",
        "w6",
        "--role-file",
        link.to_str().expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(add_c, "Z3c fixture add w6 via symlink must ok: {add_c_v}");
    let (rm_c, rm_c_v) = case.cli_json(&[
        "remove-agent",
        "w6",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--from-spec",
        "--force",
        "--confirm",
        "--json",
    ]);
    assert!(rm_c, "Z3c: remove of symlinked role must ok: {rm_c_v}");
    assert!(
        master_c.exists(),
        "Z3c: symlink target outside registry must survive"
    );
    case.shutdown();
}

/// Z4 — one-shot seat: add → use → stop(stopped=true) → remove; state/spec/tmux clean.
#[test]
#[serial(env)]
fn z4_one_shot_seat_stop_reports_stopped_true_and_all_sources_clean() {
    let case = SeatCase::start("zsr2-z4", "zsr", &["w1", "w2"]);
    let role = case.workspace.join("masters").join("rev.md");
    std::fs::write(&role, role_doc("rev")).expect("write reviewer role");
    let (add_ok, add) = case.cli_json(&[
        "add-agent",
        "rev",
        "--role-file",
        role.to_str().expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(add_ok, "Z4: add rev must ok: {add}");
    let (send_ok, send) = case.cli_json(&[
        "send",
        "rev",
        "z4 review request",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(send_ok, "Z4: the seat must be usable (send ok): {send}");

    let (stop_ok, stop) = case.cli_json(&[
        "stop-agent",
        "rev",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--json",
    ]);
    assert!(stop_ok, "Z4: stop must exit 0 AND ok=true: {stop}");
    assert_eq!(
        stop.get("stopped"),
        Some(&json!(true)),
        "Z4: stop of a live seat must report stopped=true, not a silent no-op: {stop}"
    );

    let (rm_ok, rm) = case.cli_json(&[
        "remove-agent",
        "rev",
        "--workspace",
        case.workspace_str(),
        "--team",
        "zsr",
        "--force",
        "--confirm",
        "--json",
    ]);
    assert!(rm_ok, "Z4: remove after stop must ok: {rm}");
    assert!(
        case.status_agent("rev").is_none(),
        "Z4: status must not list the seat"
    );
    assert!(
        case.pane_live("rev").is_none(),
        "Z4: tmux must hold no live pane for the seat"
    );
    assert!(
        role.exists(),
        "Z4: outside role master survives the one-shot lifecycle"
    );
    case.shutdown();
}

/// Z5 — the NEW add-agent --force surface after a real pane death: revives the
/// zombie seat, leaves the same-team peer untouched (positive isolation), and
/// (negative) never touches a same-named seat in ANOTHER team that shares the
/// SAME tmux endpoint.
///
/// r8-delta remediation: the two teams live in ONE workspace and therefore
/// share one tmux socket (verified: same-workspace teams share the endpoint),
/// so both have a window literally named `w1` in the same tmux namespace —
/// exactly the shared surface a window-only resolver would collide on. A
/// separate-socket layout (the old Z5) hid this: window names in different
/// servers never collide, so the negative assertion had no teeth. Cross-team
/// seats are tracked by PANE ID (window name is ambiguous here, which is the
/// whole point).
#[test]
#[serial(env)]
fn z5_shared_endpoint_force_recreate_isolates_same_team_peer_and_spares_cross_team_namesake() {
    // One workspace, two teams sharing one tmux endpoint; both own a `w1`.
    let case = SeatCase::start("zsr2-z5", "teamA", &["w1", "w2"]);
    case.add_second_team_same_workspace("teamB", &["w1"]);

    // Distinguish the two same-named w1 windows by pane id on the shared socket.
    let team_a_w1_before = case.pane_for_window_in_session("team-teamA", "w1");
    let team_b_w1_before = case.pane_for_window_in_session("team-teamB", "w1");
    assert!(
        team_a_w1_before.is_some() && team_b_w1_before.is_some(),
        "fixture: both teams' w1 must be live on the shared endpoint before force"
    );
    assert_ne!(
        team_a_w1_before, team_b_w1_before,
        "fixture: the two same-named w1 seats must be distinct panes on the shared socket"
    );
    let peer_before = case.pane_for_window_in_session("team-teamA", "w2");

    // Kill teamA/w1's pane → zombie, then force-recreate it.
    case.kill_pane("w1"); // teamA is the case's own team
    let (force_ok, force) = case.cli_json(&[
        "add-agent",
        "w1",
        "--role-file",
        case.workspace
            .join("agents")
            .join("w1.md")
            .to_str()
            .expect("utf8"),
        "--workspace",
        case.workspace_str(),
        "--team",
        "teamA",
        "--force",
        "--json",
    ]);
    assert!(
        force_ok,
        "Z5: add-agent --force on a zombie must exit 0 AND ok=true: {force}"
    );
    assert!(
        case.pane_for_window_in_session("team-teamA", "w1")
            .is_some(),
        "Z5: the zombie seat must be revived (live pane in team-teamA)"
    );

    // Positive isolation: same-team peer w2's pane is untouched.
    let peer_after = case.pane_for_window_in_session("team-teamA", "w2");
    assert_eq!(
        peer_before, peer_after,
        "Z5: force-recreate must not disturb the same-team peer w2 (single-seat scope)"
    );
    // Negative (teeth-bearing on the SHARED endpoint): teamB's same-named w1
    // pane is byte-identical — a window-only resolver would have killed or
    // re-bound it because the window name `w1` collides on this socket.
    let team_b_w1_after = case.pane_for_window_in_session("team-teamB", "w1");
    assert_eq!(
        team_b_w1_before, team_b_w1_after,
        "Z5 negative: force-recreate of teamA/w1 on a SHARED tmux endpoint must never touch \
         teamB's same-named w1 pane (a window-only resolver collides here); \
         before={team_b_w1_before:?} after={team_b_w1_after:?}"
    );

    case.shutdown_team("teamA");
    case.shutdown_team("teamB");
}
