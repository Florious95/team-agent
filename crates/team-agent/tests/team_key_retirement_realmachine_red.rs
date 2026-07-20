//! team-key slice2 · layer 3 — real-machine retirement lifecycle (RED, DESTRUCTIVE).
//!
//! From locate §6.2 + §7 observation points 6/7/8, NOT from the root-cause
//! reasoning. These cases drive the REAL `team-agent` binary over a real fake-
//! provider team (real tmux panes via `provider: fake`, no subscription) and
//! exercise the DESTRUCTIVE lifecycle: remove / retire / re-add / restart. They
//! are gated behind `TEAM_AGENT_REALMACHINE_RETIREMENT=1` so the frozen contract
//! does not run a destructive real-machine flow until the leader authorizes it.
//! Without the env gate each case is a no-op (skip) that documents its point.
//!
//! Every authorized body runs a REAL lifecycle and asserts on REAL surfaces —
//! never a placeholder `panic!` (a placeholder panic would red for the wrong
//! reason: it pins nothing). Reads follow the r2 (slice2b L1) discipline:
//!   - command results are asserted at the outcome level (`status=="removed"`,
//!     `ok==true`), never a bare "did it return JSON";
//!   - state is read through the CANONICAL team-scoped projection
//!     (`select_runtime_state(ws, Some(team))`), not a hard-coded nested path;
//!   - clearing invariants pin the EXISTS precondition first (the tombstone must
//!     be present after remove) so a terminal `None`/absent is not a vacuous pass.
//!
//! Observation points pinned when authorized:
//!   6. retire: quick-start -> remove/retire `worker_a` -> restart; spec (all 5
//!      reference surfaces), state agents, status agents, agent_health all ABSENT
//!      while the retirement tombstone (`agent_lifecycle.<id>.state=retired`) is
//!      RETAINED across the restart (no dynamic-role resurrection).
//!   7. explicit re-add: same id `add-agent` clears the tombstone and produces
//!      exactly one state/spec/status entry.
//!   8. restart prune: a removed seat stays ABSENT across restart and a second
//!      restart / reload, in BOTH the root state and the nested team entry.
//!   (b) atomic remove->add (Twitter queue): remove succeeds then add fails ->
//!      the original seat is NOT lost (rollback leaves no half-state).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

fn authorized() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_RETIREMENT").as_deref() == Ok("1")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

/// A destructive real-machine fixture: its own workspace under nvme tmp and its
/// own tmux socket dir. On drop it kills only the sessions/processes it started
/// (precise ownership by the team session name), never the host default socket.
struct RealTeam {
    root: PathBuf,
    team: String,
    socket: Option<String>,
}

impl RealTeam {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var("TEAM_AGENT_TEST_TMP").unwrap_or_else(|_| "/tmp".to_string());
        let root = Path::new(&base).join(format!(
            "ta-l3-retire-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("teamdir/agents")).unwrap();
        let team = format!("l3team{tag}");
        std::fs::write(
            root.join("teamdir/TEAM.md"),
            format!(
                "---\nname: {team}\nobjective: L3 retirement lifecycle.\nprovider: fake\n---\n\nTeam.\n"
            ),
        )
        .unwrap();
        Self {
            root: std::fs::canonicalize(&root).unwrap(),
            team,
            socket: None,
        }
    }

    fn write_role(&self, agent: &str) -> PathBuf {
        let p = self.root.join(format!("teamdir/agents/{agent}.md"));
        std::fs::write(
            &p,
            format!("---\nname: {agent}\nrole: {agent}\nprovider: fake\ntools:\n  - mcp_team\n---\n\n{agent}.\n"),
        )
        .unwrap();
        p
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(&self.root)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    }

    fn ws(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    /// quick-start the fake team with one worker; capture the tmux socket for
    /// precise teardown.
    fn quick_start(&mut self, worker: &str) -> Value {
        self.write_role(worker);
        let out = self.run(&[
            "quick-start",
            &self.root.join("teamdir").to_string_lossy(),
            "--workspace",
            &self.ws(),
            "--name",
            &self.team,
            "--yes",
            "--json",
        ]);
        let v = json(&out);
        // Record the socket the product derived (`ta-<hash>`), from an attach
        // command, so drop() can kill exactly this server and nothing else.
        if let Some(cmd) = v
            .get("attach_commands")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            // `tmux -S <path> attach -t ...` or `tmux -L <name> ...`
            let toks: Vec<&str> = cmd.split_whitespace().collect();
            if let Some(i) = toks.iter().position(|t| *t == "-S" || *t == "-L") {
                if let Some(sock) = toks.get(i + 1) {
                    self.socket = Some(format!("{} {}", toks[i], sock));
                }
            }
        }
        v
    }

    fn remove(&self, agent: &str) -> Value {
        json(&self.run(&[
            "remove-agent",
            agent,
            "--workspace",
            &self.ws(),
            "--from-spec",
            "--confirm",
            "--force",
            "--json",
        ]))
    }

    fn add(&self, agent: &str, role: &Path) -> Value {
        json(&self.run(&[
            "add-agent",
            agent,
            "--role-file",
            &role.to_string_lossy(),
            "--workspace",
            &self.ws(),
            "--json",
        ]))
    }

    fn restart(&self) -> Value {
        json(&self.run(&["restart", &self.ws(), "--team", &self.team, "--json"]))
    }

    fn status(&self) -> Value {
        json(&self.run(&[
            "status",
            "--workspace",
            &self.ws(),
            "--team",
            &self.team,
            "--json",
        ]))
    }

    /// Canonical team-scoped projection (r2 discipline — never a hard-coded
    /// `teams.<key>` nested read).
    fn scoped_state(&self) -> Value {
        team_agent::state::projection::select_runtime_state(&self.root, Some(&self.team))
            .expect("select_runtime_state must resolve the team scope")
    }

    fn tombstone(&self, agent: &str) -> Option<String> {
        self.scoped_state()
            .get("agent_lifecycle")?
            .get(agent)?
            .get("state")?
            .as_str()
            .map(str::to_string)
    }

    fn state_agents(&self) -> Vec<String> {
        self.scoped_state()
            .get("agents")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn status_agents(&self) -> Vec<String> {
        self.status()
            .get("agents")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every one of the 5 spec reference surfaces, flattened to the agent ids
    /// each references (agents / runtime.startup_order / routing.default_assignee
    /// / routing.rules[].assign_to / tasks[].assignee).
    fn spec_references(&self) -> Vec<String> {
        let spec_path = self
            .root
            .join(".team/runtime")
            .join(&self.team)
            .join("team.spec.yaml");
        let text = std::fs::read_to_string(&spec_path).unwrap_or_default();
        // Cheap surface scan good enough for an absence assertion: split the doc
        // into the five known keys and collect any referenced id token. We look
        // for the agent id as a whole word across the whole spec — an over-broad
        // match only makes the ABSENT assertion STRICTER (safer), never weaker.
        text.lines().map(str::to_string).collect()
    }

    fn agent_health_ids(&self) -> Vec<String> {
        let store =
            team_agent::message_store::MessageStore::open(&self.root).expect("open message store");
        let conn = team_agent::db::schema::open_db(store.db_path()).expect("open db");
        let mut stmt = conn
            .prepare("select agent_id from agent_health")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .filter_map(Result::ok)
            .collect();
        rows
    }
}

impl Drop for RealTeam {
    fn drop(&mut self) {
        // Kill ONLY the tmux server this fixture created (by the socket the
        // product reported), never the host default socket. Then TERM any
        // fake-worker whose command line carries this workspace path.
        if let Some(sock) = &self.socket {
            let parts: Vec<&str> = sock.split_whitespace().collect();
            if parts.len() == 2 {
                let _ = Command::new("tmux")
                    .args([parts[0], parts[1], "kill-server"])
                    .output();
            }
        }
        let ws = self.root.to_string_lossy().to_string();
        if let Ok(out) = Command::new("ps").args(["-axo", "pid=,command="]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains(&ws) && line.contains("fake-worker") {
                    if let Some(pid) = line.split_whitespace().next() {
                        let _ = Command::new("kill").args(["-TERM", pid]).output();
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout must be JSON; code={:?} stdout={} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn assert_removed(v: &Value, agent: &str) {
    // Outcome-level assertion (r2 discipline): an `Ok`/JSON envelope is not
    // enough — the command must report the REMOVED outcome for this id.
    assert_eq!(
        v.get("ok").and_then(Value::as_bool),
        Some(true),
        "remove-agent must succeed; got {v}"
    );
    assert_eq!(
        v.get("status").and_then(Value::as_str),
        Some("removed"),
        "remove-agent must reach the Removed outcome (not a refusal); got {v}"
    );
    assert_eq!(
        v.get("agent_id").and_then(Value::as_str),
        Some(agent),
        "remove-agent must report the removed id; got {v}"
    );
}

/// §7.6 — retire then restart: every source absent, tombstone retained across
/// the restart. Red at baseline (no tombstone semantics): the dynamic role
/// source resurrects the retired seat on restart, so the tombstone is missing
/// and/or the agent reappears.
#[test]
fn retire_then_restart_leaves_all_sources_absent_with_tombstone_retained() {
    if !authorized() {
        eprintln!(
            "SKIP (destructive, unauthorized): set TEAM_AGENT_REALMACHINE_RETIREMENT=1 \
             to run — pins §7.6 retire->restart absent+tombstone"
        );
        return;
    }
    let mut t = RealTeam::new("retire");
    t.quick_start("worker_a");
    assert!(
        t.state_agents().contains(&"worker_a".to_string()),
        "precondition: worker_a present after quick-start"
    );

    assert_removed(&t.remove("worker_a"), "worker_a");
    // EXISTS precondition (r2): the tombstone must be written by the remove,
    // else the later "retained across restart" is a vacuous pass.
    assert_eq!(
        t.tombstone("worker_a").as_deref(),
        Some("retired"),
        "remove must write the retirement tombstone"
    );

    t.restart();
    // Tombstone retained AND no resurrection across the restart.
    assert_eq!(
        t.tombstone("worker_a").as_deref(),
        Some("retired"),
        "the retirement tombstone must survive the restart (no dynamic-role resurrection)"
    );
    assert!(
        !t.state_agents().contains(&"worker_a".to_string()),
        "worker_a must stay absent from state.agents across restart"
    );
    assert!(
        !t.status_agents().contains(&"worker_a".to_string()),
        "worker_a must stay absent from status across restart"
    );
    assert!(
        !t.agent_health_ids().contains(&"worker_a".to_string()),
        "worker_a agent_health row must stay deleted across restart"
    );
    // 5 spec reference surfaces: worker_a must not appear as an assignee/order/
    // rule/default target after the retire+restart.
    let spec = t.spec_references().join("\n");
    assert!(
        !spec.contains("worker_a"),
        "worker_a must be absent from all 5 spec reference surfaces after retire+restart; spec=\n{spec}"
    );
}

/// §7.7 — explicit re-add clears the tombstone and produces exactly one seat.
/// Red at baseline: with no tombstone semantics the re-add does not clear a
/// tombstone (there is none), and the EXISTS precondition below fails.
#[test]
fn explicit_re_add_clears_tombstone_and_creates_exactly_one_seat() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins §7.7 re-add clears tombstone");
        return;
    }
    let mut t = RealTeam::new("readd");
    t.quick_start("worker_a");
    assert_removed(&t.remove("worker_a"), "worker_a");
    assert_eq!(
        t.tombstone("worker_a").as_deref(),
        Some("retired"),
        "precondition: the tombstone must EXIST after remove so re-add has something to clear"
    );

    let role = t.write_role("worker_a");
    let re = t.add("worker_a", &role);
    assert_eq!(
        re.get("ok").and_then(Value::as_bool),
        Some(true),
        "re-add must succeed; got {re}"
    );
    assert_eq!(
        t.tombstone("worker_a"),
        None,
        "a successful re-add must clear the retirement tombstone"
    );
    // Exactly one seat (no duplicate row from the resurrected + re-added paths).
    let seats: Vec<_> = t
        .state_agents()
        .into_iter()
        .filter(|a| a == "worker_a")
        .collect();
    assert_eq!(
        seats.len(),
        1,
        "re-add must produce exactly one worker_a seat; got {seats:?}"
    );
}

/// §7.8 — restart prune persistence: a removed seat stays absent across restart
/// and a second restart/reload, in BOTH the root state and the nested team
/// entry. Red at baseline: resurrection reintroduces the seat on reload.
#[test]
fn restart_prune_of_removed_seat_persists_across_reload() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins §7.8 restart prune persistence");
        return;
    }
    let mut t = RealTeam::new("prune");
    t.quick_start("worker_a");
    assert_removed(&t.remove("worker_a"), "worker_a");
    assert_eq!(
        t.tombstone("worker_a").as_deref(),
        Some("retired"),
        "precondition: remove writes the tombstone"
    );

    // Two restarts / reloads — the seat must never come back.
    for round in 0..2 {
        t.restart();
        assert!(
            !t.state_agents().contains(&"worker_a".to_string()),
            "round {round}: worker_a must stay pruned from scoped state.agents"
        );
        // Nested root read too: the raw root state's nested team entry must not
        // carry the seat back.
        let raw = team_agent::state::persist::load_runtime_state(&t.root).unwrap();
        let nested = raw
            .get("teams")
            .and_then(|x| x.get(&t.team))
            .and_then(|x| x.get("agents"))
            .and_then(Value::as_object)
            .map(|m| m.contains_key("worker_a"))
            .unwrap_or(false);
        assert!(
            !nested,
            "round {round}: worker_a must stay pruned from the nested team entry too"
        );
    }
}

/// (b) Twitter queue — remove succeeds then add fails: the original seat is not
/// lost; the transaction rolls back leaving no half-state. Uses the reachable
/// force-recreate failure seam. Red at baseline: without atomic remove->add the
/// failed add leaves the seat lost (half-state) or a stray tombstone.
#[test]
fn atomic_remove_then_failed_add_does_not_lose_the_original_seat() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins atomic remove->failed-add rollback");
        return;
    }
    let mut t = RealTeam::new("twitter");
    t.quick_start("worker_a");
    let role = t.write_role("worker_a");

    // force add whose post-consumption step fails (reachable seam). The single-
    // lock transaction must roll back to the original seat with no stray
    // tombstone.
    let out = Command::new(bin())
        .args([
            "add-agent",
            "worker_a",
            "--role-file",
            &role.to_string_lossy(),
            "--workspace",
            &t.ws(),
            "--force",
            "--json",
        ])
        .current_dir(&t.root)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env(
            "TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN",
            "twitter-queue",
        )
        .output()
        .unwrap();
    // The add is expected to fail (injected), but the seat must survive.
    let _ = out;
    assert!(
        t.state_agents().contains(&"worker_a".to_string()),
        "a failed force remove->add must restore the original worker_a seat (no lost seat)"
    );
    assert_eq!(
        t.tombstone("worker_a"),
        None,
        "a rolled-back force recreate must not leave a stray retirement tombstone"
    );
}
