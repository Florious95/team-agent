//! leader-inbound-attach-split · layer 3 — real-machine leader-channel
//! revalidation (RED, DESTRUCTIVE, mixed-real).
//!
//! From locate §8 observation points (foreign-socket refusal ⑤ / no-attach-event
//! revalidation ④), NOT the root-cause reasoning. A prior fake-transport attempt
//! was UNREACHABLE: the leader-recipient revalidation self-builds a real tmux
//! transport from `receiver.tmux_socket` (delivery.rs `delivery_transport_for_
//! recipient` :1614/1627) and `resolve_live_leader_channel` :73 requires an
//! ABSOLUTE socket path — so a fake transport is discarded and the row stays
//! queued for the WRONG reason (verifier-batch-freeze-plan.md «批1 结构性可达孔»).
//! leader ruled A (msg_03508e72aec8): ⑤④ belong to the real-machine tier.
//!
//! Mixed-real form (leader-approved msg_08c4f9f00fd0): a REAL tmux fixture
//! (quick-start → real absolute-path socket + real pane; attach-leader binds the
//! real pane as the leader receiver) drives the PUBLIC delivery entrypoint
//! `deliver_pending_message` with the REAL state (real socket) and a REAL tmux
//! transport. Because the socket is a real absolute path pointing at the real
//! server, the self-built revalidation transport actually probes the real pane —
//! so the verdict reflects true socket authority, not fake-transport inertia.
//!
//! Gated behind `TEAM_AGENT_REALMACHINE_LEADER_INBOUND=1`; unauthorized => skip.
//!
//! r2 discipline: outcome asserted at the DB row-status level (read straight back
//! from `messages`, no intervening status probe); the EXISTS precondition pins
//! the row is `queued_until_leader_attach` BEFORE the deliver; and a POSITIVE
//! CONTROL (leader-approved, per the new seventh 六查 check) proves the candidate
//! DOES revalidate a genuinely-live channel — so ④'s "stays queued" is a
//! socket-authority verdict, not a candidate that refuses to ever revalidate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::Value;

use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{deliver_pending_message, enqueue_leader_mailbox_until_attach};

fn authorized() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_LEADER_INBOUND").as_deref() == Ok("1")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

/// A destructive real-machine team fixture: its own workspace under nvme tmp and
/// its own tmux socket. On drop it kills ONLY the tmux server it created (by the
/// absolute socket the product reported) and TERMs only fake-workers whose
/// command line carries this workspace — never the host default socket.
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
            "ta-l3-leaderinbound-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("teamdir/agents")).unwrap();
        let team = format!("l3li{tag}");
        std::fs::write(
            root.join("teamdir/TEAM.md"),
            format!(
                "---\nname: {team}\nobjective: L3 leader-inbound revalidation.\nprovider: fake\n---\n\nTeam.\n"
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

    /// quick-start the fake team with one worker; capture the absolute tmux
    /// socket (`-S <path>`) for the revalidation transport AND precise teardown.
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
        if let Some(cmd) = v
            .get("attach_commands")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            let toks: Vec<&str> = cmd.split_whitespace().collect();
            if let Some(i) = toks.iter().position(|t| *t == "-S") {
                if let Some(sock) = toks.get(i + 1) {
                    self.socket = Some((*sock).to_string());
                }
            }
        }
        v
    }

    /// The absolute socket path the product derived for this team's tmux server.
    fn socket(&self) -> &str {
        self.socket
            .as_deref()
            .expect("quick-start must have reported an absolute -S socket")
    }

    /// The live pane ids on this fixture's own tmux server.
    fn live_pane_ids(&self) -> Vec<String> {
        let out = Command::new("tmux")
            .args(["-S", self.socket(), "list-panes", "-a", "-F", "#{pane_id}"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Bind a real pane as the leader receiver (status=attached, real socket,
    /// real pane id, real fingerprint) via the real CLI.
    fn attach_leader(&self, pane: &str) -> Value {
        json(&self.run(&[
            "attach-leader",
            "--workspace",
            &self.ws(),
            "--team",
            &self.team,
            "--pane",
            pane,
            "--provider",
            "claude",
            "--confirm",
            "--json",
        ]))
    }

    /// Kill exactly one pane on this fixture's own server (precise ownership).
    fn kill_pane(&self, pane: &str) {
        let _ = Command::new("tmux")
            .args(["-S", self.socket(), "kill-pane", "-t", pane])
            .output();
    }

    /// The real runtime state (root), as the delivery path reads it.
    fn state(&self) -> Value {
        team_agent::state::persist::load_runtime_state(&self.root).unwrap()
    }

    /// Enqueue one leader-mailbox row (the only entry that yields a
    /// `queued_until_leader_attach` row) into THIS workspace and return its id.
    fn enqueue_leader_row(&self, content: &str) -> String {
        let event_log = EventLog::new(&self.root);
        let outcome = enqueue_leader_mailbox_until_attach(
            &self.root, &self.team, content, None, "worker_a", &event_log,
        )
        .expect("enqueue leader mailbox row");
        outcome
            .message_id
            .expect("offline mailbox enqueue must return a message id")
    }

    fn row_status(&self, message_id: &str) -> Option<String> {
        let store = MessageStore::open(&self.root).unwrap();
        let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
        conn.query_row(
            "select status from messages where message_id = ?1",
            params![message_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }
}

impl Drop for RealTeam {
    fn drop(&mut self) {
        if let Some(sock) = &self.socket {
            let _ = Command::new("tmux")
                .args(["-S", sock, "kill-server"])
                .output();
        }
        // TERM every process whose command line carries THIS workspace path —
        // precise ownership by the unique nvme workspace token (never a bare
        // name/pkill). This covers fake-workers, the coordinator, AND the leader
        // pane's `team-agent` process (attach-leader binds a real pane), so no
        // process keeps a file handle open under root when we remove it below.
        // Poll until no owned process remains (bounded), because TERM is async
        // and quick-start's coordinator settles a beat after the signal.
        let ws = self.root.to_string_lossy().to_string();
        let owned_pids = || -> Vec<String> {
            let mut pids = Vec::new();
            if let Ok(out) = Command::new("ps").args(["-axo", "pid=,command="]).output() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if line.contains(&ws) {
                        if let Some(pid) = line.split_whitespace().next() {
                            pids.push(pid.to_string());
                        }
                    }
                }
            }
            pids
        };
        for _ in 0..25 {
            let pids = owned_pids();
            if pids.is_empty() {
                break;
            }
            for pid in &pids {
                let _ = Command::new("kill").args(["-TERM", pid]).output();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Even after every owned process is gone, a just-terminated coordinator
        // may have left an in-flight write; retry the remove until it sticks.
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.root).is_ok() && !self.root.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
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

/// Deliver the queued leader row against the real state + a REAL tmux transport
/// bound to the fixture's own socket, then read the row status straight back.
fn deliver_and_read(t: &RealTeam, message_id: &str) -> Option<String> {
    let store = MessageStore::open(&t.root).unwrap();
    let event_log = EventLog::new(&t.root);
    let state = t.state();
    let transport = team_agent::transport_factory::tmux_endpoint_transport(t.socket());
    let _ = deliver_pending_message(&t.root, &store, &transport, message_id, &event_log, &state);
    t.row_status(message_id)
}

/// ④ receiver marked attached but its pane is NOT live: attach-leader binds the
/// real pane, then the pane is killed. The receiver row still says
/// `status=attached`, but a liveness probe over the real socket finds no live
/// pane. The candidate keeps the leader row queued (PaneNotLive); the parent
/// (status-string only) flips it to accepted/failed — a physical mis-delivery.
#[test]
fn attached_receiver_with_dead_pane_must_not_revalidate_leader_row() {
    if !authorized() {
        eprintln!(
            "SKIP (destructive, unauthorized): set TEAM_AGENT_REALMACHINE_LEADER_INBOUND=1 \
             to run — pins §8 ④ dead-pane must not revalidate the leader mailbox row"
        );
        return;
    }
    let mut t = RealTeam::new("deadpane");
    t.quick_start("worker_a");
    let panes = t.live_pane_ids();
    let pane = panes
        .first()
        .expect("quick-start must leave at least one live pane")
        .clone();

    // Bind the real pane as the attached leader receiver (real socket, real id).
    let attached = t.attach_leader(&pane);
    assert_eq!(
        attached.get("ok").and_then(Value::as_bool),
        Some(true),
        "attach-leader must bind the real pane as leader receiver; got {attached}"
    );

    // Enqueue a leader mailbox row, then verify the EXISTS precondition.
    let message_id = t.enqueue_leader_row("inbound to soon-dead leader pane");
    assert_eq!(
        t.row_status(&message_id).as_deref(),
        Some("queued_until_leader_attach"),
        "precondition: the leader row must be queued_until_leader_attach before revalidation"
    );

    // Kill the leader pane: the receiver still records status=attached, but the
    // real socket now hosts no live pane for that id.
    t.kill_pane(&pane);

    let status = deliver_and_read(&t, &message_id);
    assert_eq!(
        status.as_deref(),
        Some("queued_until_leader_attach"),
        "an attached receiver whose real pane is dead must NOT revalidate the leader row: a real \
         liveness probe over the recorded socket (not the status string) governs the flip. \
         got {status:?}"
    );
}

/// POSITIVE CONTROL (leader-approved seventh 六查 check — anchors ④ to socket
/// authority, not a blanket keep-queued). The real pane stays LIVE: attach-leader
/// binds it and it is NOT killed, so a liveness probe over the real socket finds
/// it. The candidate MUST revalidate — the row must LEAVE
/// `queued_until_leader_attach`. If this asserted-green case also stayed queued,
/// ④ would prove nothing (the candidate would just never revalidate).
#[test]
fn live_pane_matching_socket_does_revalidate_leader_row() {
    if !authorized() {
        eprintln!(
            "SKIP (destructive, unauthorized): pins the positive control — a live real pane \
             on the recorded socket DOES revalidate (leaves queued_until_leader_attach)"
        );
        return;
    }
    let mut t = RealTeam::new("livepane");
    t.quick_start("worker_a");
    let panes = t.live_pane_ids();
    let pane = panes
        .first()
        .expect("quick-start must leave at least one live pane")
        .clone();

    let attached = t.attach_leader(&pane);
    assert_eq!(
        attached.get("ok").and_then(Value::as_bool),
        Some(true),
        "attach-leader must bind the real pane; got {attached}"
    );

    let message_id = t.enqueue_leader_row("inbound to live leader pane");
    assert_eq!(
        t.row_status(&message_id).as_deref(),
        Some("queued_until_leader_attach"),
        "precondition: the leader row must be queued_until_leader_attach before revalidation"
    );

    // Pane stays LIVE — do NOT kill it.
    let status = deliver_and_read(&t, &message_id);
    assert_ne!(
        status.as_deref(),
        Some("queued_until_leader_attach"),
        "a live real pane on the recorded absolute socket IS a live leader channel: the candidate \
         must revalidate (leave queued_until_leader_attach). If this stayed queued, ④'s \
         'stays queued' verdict would not be attributable to socket authority. got {status:?}"
    );
}
