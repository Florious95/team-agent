//! agent-clone-fork · successor RED batch 3 (verifier) — R4 parallel N +
//! R5 role-lifecycle rollback teeth + R6 source-immutability GUARDRAIL.
//!
//! From locate.md §7 R4/R5 + §3 surface (fork state row / window / pane
//! `fork_agent.rs:146-162,183`, remove/rollback `restart/remove.rs`,
//! `fork_agent.rs:334-453`) + §4.4 (parallel-N transaction) + §5 observation
//! points 6/9/10, NOT the §4 root-cause design. Baseline `9e6be51` (v0.5.52).
//!
//! Each Case seeds the SOURCE session tuple (a hermetic PATH-shim has no real
//! captured backing) so the fork reaches its success path; the RED is about the
//! NEW identities and the failure-path residue, not the source's backing.
//!
//! - **R4 parallel N** (identity + backing uniqueness under REAL concurrency):
//!   forking the same source N=3 times, launched SIMULTANEOUSLY via a barrier,
//!   must yield pairwise-distinct NEW `session_id`s AND pairwise-distinct backings
//!   (each non-null, none equal to the source). Baseline red: every fork reports
//!   ok with `session_id=null` (locate §3 success/readback; batch2 R2), so N forks
//!   are indistinguishable as forked contexts. Window/pane uniqueness is a
//!   POSITIVE CONTROL (baseline already assigns them distinct) — it must stay
//!   green, guarding against a "reject everything" false red. The concurrency is a
//!   real barrier (leader ruling: the parallel axis is a user red line, not a
//!   sequential loop); provider-side lock-timeout semantics remain E2E (§5.8).
//!
//! - **R5 role-lifecycle rollback teeth** (failure-path residue): when the
//!   provider backing cannot converge (source `rollout_path` points at a missing
//!   file — the copy source is gone), the fork MUST fail and roll back its
//!   reservation, leaving NO orphan `status:running` state row (locate §4.4 step
//!   4, §5 obs 10, §3:183 "marks running before session capture"). Baseline red:
//!   the fork reports exit 0 / ok:true and leaves an orphan `status:running` row.
//!   This is a DIFFERENT face from batch2 R3: R3 bites the report `ok` boolean for
//!   a spawn-without-transcript; R5 bites the physical residue (an un-rolled-back
//!   running row) when the copy source itself is missing.
//!
//! - **R6 source immutability GUARDRAIL** (naturally-green, not a RED): baseline
//!   fork copies from the compiled spec and never touches the source, so a fork
//!   already leaves the source's rollout file, its state session tuple, and its
//!   role file byte-identical (measured). There is no offline structural red to
//!   prove, so — following the L1 r2 "honestly-labelled naturally-green
//!   guardrail" precedent — this batch pins those invariants as a GUARDRAIL: it
//!   passes at baseline and its teeth engage AFTER implement swaps in the real
//!   backing copy (locate §6.5) — any implementation that damages the source
//!   during a fork goes red on the spot, without waiting for the real-machine
//!   gate. The true SEMANTICS ("source resume still recalls its own nonce; source
//!   backing carries no clone writes") remains a subscription-E2E concern (locate
//!   §4.5, §5 obs 11), asserted there, not offline.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch3";
const SOURCE: &str = "src_worker";

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: Option<PathBuf>,
}

impl Case {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace("ws");
        write_team_docs(&workspace);
        let shim_dir = write_claude_shim(&workspace);
        let shim_path = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").expect("PATH present")
        );
        let mut case = Self {
            env,
            workspace,
            shim_path,
            socket: None,
        };
        case.quick_start();
        case
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("ws utf8")
    }

    fn rollout_path(&self) -> PathBuf {
        self.workspace.join("fixture-source-rollout.jsonl")
    }

    fn state_path(&self) -> PathBuf {
        self.workspace
            .join(".team")
            .join("runtime")
            .join("state.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.env
            .run_cli_env(&self.workspace, args, &[("PATH", self.shim_path.as_str())])
    }

    fn quick_start(&mut self) {
        let out = self.run(&[
            "quick-start",
            self.ws(),
            "--workspace",
            self.ws(),
            "--name",
            TEAM_NAME,
            "--yes",
            "--json",
        ]);
        if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
            if let Some(cmd) = v
                .get("attach_commands")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.as_str())
                .or_else(|| v.get("leader_attach_command").and_then(|c| c.as_str()))
            {
                let toks: Vec<&str> = cmd.split_whitespace().collect();
                if let Some(i) = toks.iter().position(|t| *t == "-S") {
                    if let Some(sock) = toks.get(i + 1) {
                        self.socket = Some(PathBuf::from(*sock));
                    }
                }
            }
        }
    }

    /// Seed the SOURCE backing tuple so the fork has a valid source to copy from.
    /// `rollout` is where the source's captured rollout lives; pointing it at a
    /// missing path is the R5 failure injection.
    fn seed_source_tuple(&self, rollout: &Path) {
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return;
        };
        let Ok(mut state) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        let tuple = json!({
            "session_id": "sess-cf-batch3-source",
            "rollout_path": rollout.to_string_lossy(),
            "captured_at": "2026-07-21T00:00:00Z",
            "captured_via": "contract-fixture"
        });
        let mut patch = |row: &mut Value| {
            if let Some(obj) = row.as_object_mut() {
                for (k, v) in tuple.as_object().unwrap() {
                    obj.insert(k.clone(), v.clone());
                }
            }
        };
        if let Some(row) = state.get_mut("agents").and_then(|a| a.get_mut(SOURCE)) {
            patch(row);
        }
        if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
            for team in teams.values_mut() {
                if let Some(row) = team.get_mut("agents").and_then(|a| a.get_mut(SOURCE)) {
                    patch(row);
                }
            }
        }
        let _ = std::fs::write(
            self.state_path(),
            serde_json::to_string_pretty(&state).unwrap(),
        );
    }

    fn fork(&self, as_name: &str) -> Value {
        let out = self.run(&[
            "fork-agent",
            SOURCE,
            "--as",
            as_name,
            "--workspace",
            self.ws(),
            "--team",
            TEAM_NAME,
            "--no-display",
            "--json",
        ]);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            json!({
                "ok": out.status.success(),
                "_exit": out.status.code(),
                "_stderr": String::from_utf8_lossy(&out.stderr),
            })
        })
    }

    /// Read the state.json agent rows for the selected team (teams[..].agents).
    fn team_agent_rows(&self) -> serde_json::Map<String, Value> {
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return serde_json::Map::new();
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return serde_json::Map::new();
        };
        if let Some(teams) = v.get("teams").and_then(Value::as_object) {
            for team in teams.values() {
                if let Some(agents) = team.get("agents").and_then(Value::as_object) {
                    if agents.contains_key(SOURCE) {
                        return agents.clone();
                    }
                }
            }
        }
        serde_json::Map::new()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if let Some(sock) = &self.socket {
            let _ = std::process::Command::new("tmux")
                .args(["-S", sock.to_str().unwrap_or(""), "kill-server"])
                .output();
        }
        let ws = self.workspace.to_string_lossy().to_string();
        if let Ok(out) = std::process::Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains(&ws) {
                    if let Some(pid) = line.split_whitespace().next() {
                        let _ = std::process::Command::new("kill")
                            .args(["-TERM", pid])
                            .output();
                    }
                }
            }
        }
        let _ = &self.env;
    }
}

fn write_team_docs(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch3.\nprovider: claude\n---\n"),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{SOURCE} body.\n"
        ),
    )
    .expect("write source role doc");
}

/// Success-path shim that produces a DISTINCT NEW backing per fork. Each fork
/// spawn carries a predetermined `--session-id <uuid>` on argv (adapter.rs:478);
/// the shim writes a claude transcript at the exact path the session scanner reads
/// (`$HOME/.claude/projects/<encoded-cwd>/<uuid>.jsonl`, encoding per
/// session_scan/claude.rs: every non-alphanumeric char → '-'), keyed to that
/// uuid. This lets each of the N forks capture a distinct verified NEW backing —
/// without it, a spawn-only shim produces no NEW backing and the MUST-17
/// implementation refuses every fork (context_fork_unverified), which would make
/// R4's uniqueness assertions unreachable (vacuous green — the false-green-hole
/// runtime-owner surfaced). The RED cause is unchanged: at baseline the fork
/// reports session_id=null regardless of the emitted backing.
fn write_claude_shim(workspace: &Path) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    std::fs::write(
        &shim,
        r#"#!/bin/sh
sid=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ]; then sid="$a"; fi
  prev="$a"
done
if [ -n "$sid" ]; then
  enc=$(printf '%s' "$PWD" | sed 's/[^a-zA-Z0-9]/-/g')
  dir="$HOME/.claude/projects/$enc"
  mkdir -p "$dir"
  printf '{"sessionId":"%s","type":"fork-backing"}\n' "$sid" > "$dir/$sid.jsonl"
fi
echo 'claude shim ready'
exec sleep 3600
"#,
    )
    .expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

fn new_session_of(row: &Value) -> Option<String> {
    row.get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn window_of(row: &Value) -> Option<String> {
    row.get("window")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn pane_of(row: &Value) -> Option<String> {
    row.get("pane_id")
        .or_else(|| row.get("pane"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn backing_of(row: &Value) -> Option<String> {
    row.get("rollout_path")
        .or_else(|| row.get("backing_path"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// R4 — forking the same source N=3 must yield pairwise-distinct NEW session ids
/// (each non-null, none equal to the source). Baseline red: every fork reports ok
/// with session_id=null, so the N forks are indistinguishable as forked contexts.
///
/// Window/pane uniqueness is a POSITIVE CONTROL: baseline already assigns them
/// distinct, and it must stay green — this guards against a degenerate "reject
/// everything" implementation passing the session assertion for the wrong reason.
///
/// REVISION 2 (2026-07-21, leader ruling msg_432066100c50): the parallel-N axis
/// is a user red line and must NOT be downgraded to a sequential loop — locate §7
/// R4's prototype is "barrier 同时发 N=3". A sequential loop only proves
/// repeatability; concurrency isolation (workspace-lock contention, reservation
/// mutual-exclusion, identity uniqueness under race) is only exercised when the N
/// forks are launched SIMULTANEOUSLY. This revision uses a real `Barrier` +
/// scoped threads so all N `fork-agent` subprocesses race, and asserts identity
/// AND backing uniqueness directly: session_id + rollout_path/backing pairwise
/// distinct + ownership (locate §5.6). (True provider-side lock-timeout /
/// one-seat-failure-doesn't-block-others is a subscription-E2E concern, §5.8 —
/// not asserted offline.)
///
/// REVISION 1 (implement interlock — runtime-owner surfaced a vacuous green): the
/// original shim only spawned (no NEW backing), so the MUST-17 implementation
/// refused every fork and the `all_ok` early-return skipped ALL uniqueness
/// assertions — a vacuous green (false-green-hole type 8: assertion unreachable).
/// The shim now emits a distinct NEW backing per fork (keyed to the predetermined
/// --session-id) and every fork MUST report ok.
///
/// RED cause unchanged @baseline: each fork reports ok:true (baseline never checks
/// backing) with session_id=null, so the uniqueness assertions fire.
#[test]
fn r4_parallel_n_forks_have_pairwise_unique_new_sessions() {
    let case = Case::start("cf-r4");
    case.seed_source_tuple(&case.rollout_path());
    std::fs::write(case.rollout_path(), "{\"type\":\"fixture-source\"}\n").expect("write rollout");

    let names = ["f1", "f2", "f3"];
    // REAL concurrency: a barrier releases all N fork subprocesses simultaneously
    // so workspace-lock contention / reservation mutual-exclusion is actually
    // exercised (a sequential loop would not). Scoped threads borrow `&case`.
    let barrier = std::sync::Barrier::new(names.len());
    let forks: Vec<(&str, Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = names
            .iter()
            .map(|&n| {
                let barrier = &barrier;
                let case = &case;
                scope.spawn(move || {
                    barrier.wait();
                    (n, case.fork(n))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Every fork MUST report ok — no early-return escape that would leave the
    // uniqueness assertions unreachable (vacuous green, type 8).
    for (n, fork) in &forks {
        assert_eq!(
            fork.get("ok").and_then(Value::as_bool),
            Some(true),
            "fork {n} must report ok so the N-way uniqueness assertions are reachable (no vacuous \
             green). A refusal of a legitimate concurrent fork is itself the failure. fork={fork}"
        );
    }

    let rows = case.team_agent_rows();
    let source_session = rows.get(SOURCE).and_then(new_session_of);

    let uniq = |v: &[String]| {
        let mut s = v.to_vec();
        s.sort();
        s.dedup();
        s.len() == v.len()
    };

    // POSITIVE CONTROL: window + pane must be pairwise distinct across forks.
    // Baseline satisfies this; it must never regress (guards against a
    // reject-everything impl passing the session assertion for the wrong reason).
    let mut windows: Vec<String> = Vec::new();
    let mut panes: Vec<String> = Vec::new();
    for n in names {
        let row = rows
            .get(n)
            .unwrap_or_else(|| panic!("fork {n} row present"));
        if let Some(w) = window_of(row) {
            windows.push(w);
        }
        if let Some(p) = pane_of(row) {
            panes.push(p);
        }
    }
    assert!(
        windows.len() == names.len() && uniq(&windows),
        "positive control: N forks must have pairwise-distinct windows; windows={windows:?}"
    );
    assert!(
        panes.len() == names.len() && uniq(&panes),
        "positive control: N forks must have pairwise-distinct panes; panes={panes:?}"
    );

    // R4 red: NEW session ids must be present, pairwise distinct, and != source.
    let mut new_sessions: Vec<String> = Vec::new();
    for n in names {
        let row = rows.get(n).unwrap();
        let sess = new_session_of(row).unwrap_or_else(|| {
            panic!(
                "fork {n} reported ok but carries no NEW session id (session_id=null): N concurrent \
                 forks are indistinguishable as forked contexts (locate §3/§4.4; batch2 R2). row={row}"
            )
        });
        assert_ne!(
            Some(&sess),
            source_session.as_ref(),
            "fork {n} NEW session id must differ from the source; got {sess}"
        );
        new_sessions.push(sess);
    }
    assert!(
        uniq(&new_sessions),
        "N concurrent forks must have PAIRWISE-DISTINCT new session ids; got {new_sessions:?}"
    );

    // R4 backing uniqueness (leader ruling msg_432066100c50 §2, locate §5.6): each
    // fork must own a DISTINCT backing, not merely a distinct session id — two
    // forks with different session ids pointing at the same backing rollout would
    // be false-independent. Assert rollout_path/backing pairwise distinct, != the
    // source's, and non-empty.
    let source_backing = rows.get(SOURCE).and_then(backing_of);
    let mut backings: Vec<String> = Vec::new();
    for n in names {
        let row = rows.get(n).unwrap();
        let backing = backing_of(row).unwrap_or_else(|| {
            panic!(
                "fork {n} reported ok but carries no NEW backing (rollout_path/backing_path empty): a \
                 forked context must own an independent backing, not just a distinct id (locate §5.6). \
                 row={row}"
            )
        });
        assert_ne!(
            Some(&backing),
            source_backing.as_ref(),
            "fork {n} backing must differ from the source's backing; got {backing}"
        );
        backings.push(backing);
    }
    assert!(
        uniq(&backings),
        "N concurrent forks must own PAIRWISE-DISTINCT backings (independent context, not shared \
         rollout); got {backings:?}"
    );
}

/// Snapshot of the SOURCE-owned bytes that a fork MUST NOT mutate: the source
/// rollout file contents, the source state session tuple, and the source role
/// file contents.
fn source_snapshot(case: &Case) -> (Vec<u8>, Value, Vec<u8>) {
    let rollout = std::fs::read(case.rollout_path()).unwrap_or_default();
    let rows = case.team_agent_rows();
    let tuple = rows
        .get(SOURCE)
        .map(|row| {
            json!({
                "session_id": row.get("session_id").cloned(),
                "rollout_path": row.get("rollout_path").cloned(),
                "captured_at": row.get("captured_at").cloned(),
                "captured_via": row.get("captured_via").cloned(),
            })
        })
        .unwrap_or(Value::Null);
    let role = std::fs::read(case.workspace.join("agents").join(format!("{SOURCE}.md")))
        .unwrap_or_default();
    (rollout, tuple, role)
}

/// R6 source-immutability GUARDRAIL — a fork must leave the SOURCE untouched:
/// its rollout file, its state session tuple, and its role file must be
/// byte/field-identical before and after. Baseline is NATURALLY GREEN (fork copies
/// from the compiled spec and never touches the source). This is not a RED; it is
/// a guardrail whose teeth engage after implement swaps in the real backing copy
/// (locate §6.5) — any impl that writes into the source during a fork trips it.
#[test]
fn r6_fork_leaves_source_backing_and_role_unmutated_guardrail() {
    let case = Case::start("cf-r6");
    std::fs::write(
        case.rollout_path(),
        "{\"type\":\"fixture-source\",\"nonce\":\"SRC_KEEP\"}\n",
    )
    .expect("write source rollout");
    case.seed_source_tuple(&case.rollout_path());

    let before = source_snapshot(&case);
    let fork = case.fork("f1");
    // The guardrail holds regardless of whether the fork succeeded; what it pins
    // is that the SOURCE was not mutated as a side effect.
    let _ = fork;
    let after = source_snapshot(&case);

    assert_eq!(
        before.0, after.0,
        "guardrail: fork must not mutate the source rollout file (copy source, not copy target); \
         a fork that writes into the source rollout has damaged the source backing (locate §4.5)."
    );
    assert_eq!(
        before.1, after.1,
        "guardrail: fork must not mutate the source state session tuple \
         (session_id/rollout_path/captured_at/captured_via must be field-identical). before={} after={}",
        before.1, after.1
    );
    assert_eq!(
        before.2, after.2,
        "guardrail: fork must not mutate the source role file."
    );
}

/// R5 role-lifecycle rollback teeth — when the provider backing cannot converge
/// (the source rollout_path points at a MISSING file, so there is nothing valid
/// to copy), the fork MUST fail and roll back, leaving NO orphan `status:running`
/// state row. Baseline red: the fork reports exit 0 / ok:true and leaves an
/// orphan running row (locate §4.4 step 4, §5 obs 10; §3:183 marks running before
/// session capture).
///
/// Distinct from batch2 R3: R3 bites the report `ok` boolean for a
/// spawn-without-transcript; R5 bites the physical residue (an un-rolled-back
/// `status:running` row) when the copy source itself is missing.
#[test]
fn r5_failed_fork_rolls_back_leaving_no_running_orphan() {
    let case = Case::start("cf-r5");
    // Failure injection: source declares a rollout_path that does not exist.
    let missing = case.workspace.join("does-not-exist-rollout.jsonl");
    assert!(
        !missing.exists(),
        "precondition: rollout target must be absent"
    );
    case.seed_source_tuple(&missing);

    let fork = case.fork("fx");
    let ok = fork.get("ok").and_then(Value::as_bool) == Some(true);

    let rows = case.team_agent_rows();
    let fx_status = rows
        .get("fx")
        .and_then(|r| r.get("status"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let orphan_running = fx_status.as_deref() == Some("running");

    // The teeth: a fork whose backing could not converge (missing copy source)
    // must NOT leave a `status:running` row. Either the command fails/rolls back
    // (fx row absent or a non-running failure/tombstone status), or — at minimum —
    // it must not report ok while parking an orphan running row. Baseline does
    // exactly the forbidden thing: ok:true AND fx status=running.
    assert!(
        !(ok && orphan_running),
        "a fork whose provider backing could NOT converge (source rollout_path missing) must roll \
         back its reservation, not leave an orphan status:running row: the old implementation marks \
         running before session capture and never rolls back (ok:true, fx status=running). \
         Expected fail/rollback (no running orphan). got ok={ok} fx_status={fx_status:?}. fork={fork}"
    );
}
