//! agent-clone-fork · successor RED batch 2 (verifier) — R2 verified truth +
//! R3 false-success teeth.
//!
//! From locate.md §7 R2/R3 + §1.2 (Codex N=2: one seat spawned but 90s no final
//! output — spawn is not success) + §3 success/readback surface, NOT the §4
//! design. Baseline `9e6be51` (v0.5.52): fork reports `ok:true` and
//! `session_id:null` (locate §3 report_type), and the success gate only checks
//! registration (pane/window), never the NEW provider backing (locate §3
//! success/readback fork_agent.rs:312-463 violates MUST-17/N36).
//!
//! - **R2 verified-OR-pending truth** (2026-07-24 A 案裁定 msg_8eee0559ca68):
//!   a fork that returns `ok:true` may be in either of TWO typed shapes ONLY:
//!     (a) Verified — carries a non-null NEW `session_id` distinct from the
//!         source (and its backing is readable), or
//!     (b) Pending — `backing_state == "pending_context_fork"` AND
//!         `session_id / new_session_id / backing_path` remain null (tuple
//!         null; seat still registered). This preserves 0.5.58 齿① typed
//!         Pending admissibility.
//!   Forbidden shapes STILL RED (伪造 detection): `ok:true` + non-null
//!   `session_id`/`new_session_id` that either equals the source OR is not
//!   accompanied by a Verified backing_state (i.e. a "helpful" retry that
//!   fabricates a tuple). NOTE: A 案 explicitly retires the previous
//!   §7 R2 "ok:true ⇒ non-null NEW session id" postulate because it
//!   encoded the synchronous-only world-view locate.md identifies as the
//!   P0 root cause; retained by-字段 predicates: (i) 伪造 tuple 必红,
//!   (ii) 禁静默降级 fresh clone.
//! - **R3 false-success + pending-vs-refuse teeth** (A 案 co-revision): a
//!   provider shim that only SPAWNS (sleeps) and produces NO NEW
//!   transcript/backing may exit in one of two typed shapes:
//!     (a) Refuse/rollback with `context_fork_unverified` (`ok:false`), or
//!     (b) Pending (`ok:true` + `backing_state=="pending_context_fork"`
//!         + tuple null + seat registered + typed grace bound).
//!   The FORBIDDEN shape (2026-07-17 false-success family): `ok:true` with
//!   a non-null `session_id`/`new_session_id` absent a Verified
//!   backing_state — i.e. fabricated tuple. Silently downgrading to a
//!   fresh-clone code path (dropping `backing_state` from the report so it
//!   looks like a Verified clone) is also RED.
//!
//! Offline structural RED (PATH-shim provider, zero tokens): the source session
//! backing is seeded as fixture prep (a hermetic shim has no real captured
//! session); this batch pins that ok requires a VERIFIED NEW backing, not that
//! the fork "remembers" the nonce (that is subscription-E2E, locate §4.5).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{json, Value};

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch2";
const SOURCE: &str = "src_worker";
const NEW: &str = "new_worker";

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    socket: Option<PathBuf>,
}

impl Case {
    /// `emit_transcript=false` is the R3 teeth shim: the provider only spawns
    /// (sleeps) and never writes a session transcript/backing.
    fn start(tag: &str, emit_transcript: bool) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace("ws");
        write_team_docs(&workspace);
        let shim_dir = write_claude_shim(&workspace, emit_transcript);
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
        case.seed_source_session_tuple();
        case
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().expect("ws utf8")
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

    /// Seed the SOURCE backing tuple (a hermetic shim has no real captured
    /// session) so the fork has a valid source to copy from. The RED is about
    /// the NEW backing, not the source's.
    fn seed_source_session_tuple(&self) {
        let rollout = self.workspace.join("fixture-source-rollout.jsonl");
        std::fs::write(&rollout, "{\"type\":\"fixture-source\"}\n").expect("write source rollout");
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return;
        };
        let Ok(mut state) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        let tuple = json!({
            "session_id": "sess-cf-batch2-source",
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

    fn fork(&self) -> Value {
        let out = self.run(&[
            "fork-agent",
            SOURCE,
            "--as",
            NEW,
            "--workspace",
            self.ws(),
            "--team",
            TEAM_NAME,
            "--no-display",
            "--json",
        ]);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "fork-agent --json must emit JSON; stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
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
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch2.\nprovider: claude\n---\n"),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: claude\nmodel: claude-sonnet-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{SOURCE}.\n"
        ),
    )
    .expect("write source role doc");
}

fn write_claude_shim(workspace: &Path, emit_transcript: bool) -> PathBuf {
    let bin_dir = workspace.join("shim-bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim dir");
    let shim = bin_dir.join("claude");
    // R3 teeth: with emit_transcript=false the shim ONLY spawns (sleeps) and
    // never writes any session backing — a spawn-without-transcript that the old
    // implementation would wrongly accept as a forked context.
    let body = if emit_transcript {
        "#!/bin/sh\nmkdir -p \"$HOME/.claude/projects/shim\"\necho '{\"type\":\"shim\"}' > \"$HOME/.claude/projects/shim/session.jsonl\"\necho 'claude shim ready'\nexec sleep 3600\n"
    } else {
        "#!/bin/sh\necho 'claude shim ready'\nexec sleep 3600\n"
    };
    std::fs::write(&shim, body).expect("write claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude shim");
    }
    bin_dir
}

/// Helper: classify the fork report into one of the FOUR admissible shapes
/// under A-案 (msg_8eee0559ca68). Returns `Err(msg)` for forbidden shapes.
///
/// Admissible:
///   (V)  Verified              — ok:true + backing_state=="verified"
///        (or absent) + non-null session_id/new_session_id distinct from source
///   (P)  Pending                — ok:true + backing_state=="pending_context_fork"
///        + all of session_id/new_session_id/backing_path null
///   (Ru) Refuse: unverified     — ok:false, error == "context_fork_unverified"
///   (Ro) Refuse: rollback other — ok:false, error != "context_fork_unverified"
///
/// Forbidden (RED-ted):
///   (F1) Fabricated tuple       — ok:true + non-null session_id (either == source
///        OR without a Verified backing_state)
///   (F2) Silent fresh-clone     — ok:true + backing_state absent AND
///        `fell_back_to_clone` or `fresh_clone` truthy (any of the marker keys)
enum ForkShape {
    Verified { new_session_id: String },
    Pending,
    RefuseUnverified,
    RefuseOther,
}

const SOURCE_SESSION_ID: &str = "sess-cf-batch2-source";

fn classify_fork_shape(fork: &Value) -> Result<ForkShape, String> {
    let ok = fork.get("ok").and_then(Value::as_bool) == Some(true);
    let backing_state = fork
        .get("backing_state")
        .and_then(Value::as_str)
        .unwrap_or("");
    let session_id = fork
        .get("new_session_id")
        .or_else(|| fork.get("session_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let backing_path = fork
        .get("backing_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let downgraded_marker = fork
        .get("fell_back_to_clone")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || fork
            .get("fresh_clone")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || fork
            .get("downgraded_to_clone")
            .and_then(Value::as_bool)
            .unwrap_or(false);

    if !ok {
        let error = fork.get("error").and_then(Value::as_str).unwrap_or("");
        return Ok(if error == "context_fork_unverified" {
            ForkShape::RefuseUnverified
        } else {
            ForkShape::RefuseOther
        });
    }

    // ok:true. Two admissible shapes: Verified or Pending.
    if backing_state == "pending_context_fork" {
        // Pending shape MUST have tuple null.
        if session_id.is_some() || backing_path.is_some() {
            return Err(format!(
                "Pending shape (backing_state=pending_context_fork) must carry \
                 tuple null (session_id=null, backing_path=null); got \
                 session_id={:?} backing_path={:?}. §7#4 typed Pending禁伪造 tuple. \
                 fork={fork}",
                session_id, backing_path
            ));
        }
        return Ok(ForkShape::Pending);
    }

    // ok:true, not pending → must be Verified.
    let sid = match session_id {
        Some(s) => s.to_string(),
        None => {
            return Err(format!(
                "ok:true + backing_state={:?} (not pending_context_fork) but \
                 session_id is null. Either declare Pending (backing_state=\
                 \"pending_context_fork\") or produce a Verified NEW session id \
                 (locate §7 R2 revised A 案). fork={fork}",
                backing_state
            ));
        }
    };
    if sid == SOURCE_SESSION_ID {
        return Err(format!(
            "Fabricated tuple: ok:true + session_id == source ({SOURCE_SESSION_ID}); \
             a Verified fork MUST have a NEW session id distinct from source. \
             §7#4 禁伪造. fork={fork}"
        ));
    }
    if downgraded_marker {
        return Err(format!(
            "Silent fresh-clone downgrade detected while reporting ok:true with a \
             session_id: framework may not silently swap fork for clone. §7#4 禁静默降级. \
             fork={fork}"
        ));
    }
    if !backing_state.is_empty() && backing_state != "verified" {
        return Err(format!(
            "ok:true carries non-null session_id but backing_state={:?} — only \
             \"verified\" or \"pending_context_fork\" are admissible. fork={fork}",
            backing_state
        ));
    }
    Ok(ForkShape::Verified {
        new_session_id: sid,
    })
}

/// R2 — a fork that returns ok is admissible in ONE of two typed shapes ONLY
/// (A 案 msg_8eee0559ca68 revising the 0.5.53-era synchronous-only postulate):
///   (V) Verified   — non-null NEW session id distinct from source
///   (P) Pending    — backing_state=="pending_context_fork" + tuple null
/// Baseline red: fork returns ok with session_id null AND no
/// backing_state=="pending_context_fork" declared — neither typed shape.
#[test]
fn r2_fork_ok_is_typed_verified_or_pending() {
    let case = Case::start("cf-r2", true);
    let fork = case.fork();
    if fork.get("ok").and_then(Value::as_bool) != Some(true) {
        // If the product refuses (a legitimate non-ok), R2 does not apply
        // — the refuse-shape face is R3's concern.
        return;
    }
    let shape = classify_fork_shape(&fork).unwrap_or_else(|msg| panic!("{msg}"));
    match shape {
        ForkShape::Verified { new_session_id } => {
            // Verified is the strongest admissible ok shape. Reasserted the
            // distinctness lock the retired R2 relied on:
            assert_ne!(
                new_session_id, SOURCE_SESSION_ID,
                "Verified NEW session id MUST differ from source (already \
                 covered by classifier F1, but pinned for future refactor \
                 safety). fork={fork}"
            );
        }
        ForkShape::Pending => {
            // Pending is the new admissible ok shape enabled by 0.5.58 齿①.
            // Nothing further to assert here: classifier already guaranteed
            // tuple null.
        }
        ForkShape::RefuseUnverified | ForkShape::RefuseOther => {
            unreachable!("classified as ok:true; refuse arms unreachable")
        }
    }
}

/// R2b (A 案 新增) — baseline enforcement that the report SCHEMA admits
/// a `backing_state` field. Without a typed `backing_state` discriminant
/// the Pending shape has no code representation, so 齿① typed Pending
/// cannot be observed by any downstream consumer (silent Pending =
/// forbidden by §7#4). Baseline red: `ForkAgentReport` at 5b847e4 does
/// not include a `backing_state` key at all — the field is absent from
/// every fork report, which means Pending is unrepresentable.
#[test]
fn r2b_fork_report_schema_admits_typed_backing_state() {
    // We probe by inspecting a real fork report's key set. Any successful
    // baseline case will do — R2 (emit_transcript=true) is the closest
    // to the golden path.
    let case = Case::start("cf-r2b", true);
    let fork = case.fork();
    let has_backing_state = fork
        .as_object()
        .map(|o| o.contains_key("backing_state"))
        .unwrap_or(false);
    assert!(
        has_backing_state,
        "fork report schema missing `backing_state` discriminant. 0.5.58 齿① \
         typed Pending is only observable if the report exposes a \
         backing_state field taking values \"verified\" | \
         \"pending_context_fork\" (or the refuse arms). Without it, the \
         Pending arm collapses back to indistinguishable ok:true — silent \
         Pending is forbidden by §7#4. fork={fork}"
    );
}

/// R3 false-success + pending-vs-refuse teeth (A 案 co-revision) — a
/// provider shim that only SPAWNS and produces NO NEW transcript/backing
/// must NOT be reported as a Verified forked context. Admissible outcomes:
///   (P)  Pending (`backing_state==pending_context_fork` + tuple null +
///                 seat retained pending typed grace expiry), or
///   (Ru) Refuse with `context_fork_unverified`, or
///   (Ro) Other typed refuse.
/// Forbidden (RED): `ok:true` with a fabricated non-null NEW session id
/// (spawn-as-success, 2026-07-17 false-success family) OR silent
/// fresh-clone downgrade.
#[test]
fn r3_spawn_without_transcript_is_pending_or_refuse_never_fabricated() {
    let case = Case::start("cf-r3", false);
    let fork = case.fork();
    let shape = classify_fork_shape(&fork).unwrap_or_else(|msg| panic!("{msg}"));
    match shape {
        ForkShape::Verified { new_session_id } => {
            // A shim that emits no transcript CANNOT legitimately arrive at
            // Verified: the classifier already validates the session id
            // shape, but the R3 case adds the semantic guarantee that no
            // real backing was produced. If the product reports Verified
            // anyway, the false-success family has resurfaced.
            panic!(
                "spawn-only shim reported Verified with new_session_id={new_session_id:?}; \
                 no NEW backing was produced by design. This is the 2026-07-17 \
                 false-success family — spawn is not success. fork={fork}"
            );
        }
        ForkShape::Pending | ForkShape::RefuseUnverified | ForkShape::RefuseOther => {
            // All three are admissible for the R3 shim path.
        }
    }
}
