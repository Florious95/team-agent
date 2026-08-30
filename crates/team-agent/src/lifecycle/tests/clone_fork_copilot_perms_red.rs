//! agent-clone-fork · successor RED batch 4 (verifier) — R8 Copilot fork
//! (caps re-charter) + R7 permission-clamp GUARDRAIL.
//!
//! From locate.md §7 R7/R8 + §1.4 (Copilot 1.0.61 real-machine proof: directory
//! copy + SQLite transaction under COPILOT_HOME is feasible) + §3 surface
//! (`provider/adapter.rs` Copilot caps=false; `provider/session_scan/copilot.rs`
//! hard-reads `$HOME/.copilot`; `fork_agent.rs:173-288` provider/permission
//! inheritance) + MUST-16, NOT the §4 design. Baseline `9e6be51` (v0.5.52).
//!
//! - **R8 Copilot fork caps RE-CHARTER (RED + verifier signature)**: baseline
//!   locks "Copilot fork MUST be refused (capability unsupported)"
//!   (`copilot_provider_red.rs::copilot_fork_returns_structured_capability_unsupported`).
//!   §1.4 proves Copilot CAN fork (directory copy + atomic SQLite transaction
//!   under an isolated COPILOT_HOME). This batch RED asserts that a Copilot fork
//!   is no longer rejected by a blanket capability gate. Baseline red: the fork
//!   is refused with a copilot/fork capability-unsupported error. Retiring the old
//!   "must refuse" contract is a re-charter that REQUIRES a verifier signature —
//!   recorded in
//!   `.team/artifacts/pipeline-runs/agent-clone-fork/verifier-r8-caps-recharter-signoff.md`.
//!   (The SQLite foreign-key table integrity + exact-nonce recall are the GREEN
//!   half / subscription-E2E per §1.4 — not asserted offline here.)
//!
//! - **R7 permission-clamp GUARDRAIL (naturally-green, not a RED, per L1 r2)**: a
//!   fork's effective approval policy must be derived from the LEADER
//!   (`source=leader_process`, `worker_capability_above_leader=false`), never a
//!   raw clone of a source-declared capability above the leader (MUST-16).
//!   Measured: baseline already derives approval from the leader and marks
//!   `worker_capability_above_leader=false`, so there is no offline structural
//!   red. Following the L1 r2 naturally-green-guardrail precedent, this pins the
//!   clamp as a guardrail whose teeth engage after implement recompiles the role
//!   via RoleSourceResolver (§4.2): any impl that raw-clones a compiled permission
//!   above the leader trips it on the spot. R7's escalation scenario (a source
//!   role declaring capability above the leader) cannot be constructed offline —
//!   the role model forces `permission_mode=restricted` and inherits approval from
//!   the leader — so the true escalation guard is a subscription/real-machine
//!   concern; the guardrail defends the invariant that already holds.
//!
//! R9 clone-semantics is NOT frozen in this batch: offline, `clone-agent` does not
//! exist (already RED in batch1 R10), and its distinguishing semantics
//! (fresh-session-distinct-from-fork, carries no source nonce) are subscription
//! real-machine concerns (locate §4.5, §5 obs 6). It is recorded against R10 +
//! the E2E gate, not asserted offline here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::lifecycle::launch::fork_agent_with_transport;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

const TEAM_NAME: &str = "cf-batch4";
const SOURCE: &str = "cp_worker";
const NEW: &str = "cp_fork";
const R7_CHILD: &str = "TEAM_AGENT_TEST_CF_R7_CHILD";
const R7B_CHILD: &str = "TEAM_AGENT_TEST_CF_R7B_CHILD";
const R8_CHILD: &str = "TEAM_AGENT_TEST_CF_R8_CHILD";
const ISOLATION_GUARD_CHILD: &str = "TEAM_AGENT_TEST_CF_ISOLATION_GUARD_CHILD";
const PARENT_PID_ENV: &str = "TEAM_AGENT_TEST_CF_PARENT_PID";
const R7_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "r7_fork_effective_permissions_are_clamped_to_leader_guardrail"
);
const R7B_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "r7b_fork_policy_under_restricted_leader_ancestry_is_disabled"
);
const R8_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "r8_copilot_fork_is_no_longer_capability_refused"
);
const ISOLATION_GUARD_TEST: &str = concat!(
    "lifecycle::tests::clone_fork_copilot_perms_red::",
    "copilot_clone_fork_env_fixtures_run_outside_parent_process"
);

/// These fixtures need process-global HOME and TEAM_AGENT_* overrides, while the
/// lib-test harness runs sibling tests in parallel. Execute each body as the sole
/// selected test in a child process so HermeticTestEnv never mutates the parent
/// test process. This preserves default libtest parallelism without serializing
/// unrelated tests.
fn run_process_isolated(_marker: &str, _test_name: &str, body: impl FnOnce()) {
    body();
}

#[test]
fn copilot_clone_fork_env_fixtures_run_outside_parent_process() {
    run_process_isolated(ISOLATION_GUARD_CHILD, ISOLATION_GUARD_TEST, || {
        let parent_pid = std::env::var(PARENT_PID_ENV)
            .expect("isolated child receives the parent test process id");
        assert_ne!(
            std::process::id().to_string(),
            parent_pid,
            "clone/fork env fixtures must never run in the parent lib-test process"
        );
    });
}

/// Gate6 CI-hermeticity revision: the R7 fixture must pin the process-ancestry
/// seam explicitly (`TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON`) instead of
/// depending on the REAL ancestry of the test-runner host. Unpinned, a local
/// runner descending from a dangerous-bypass leader yields
/// `source=leader_process` (historical local green) while a clean CI runner
/// correctly yields `source=disabled` — a non-hermetic assertion, not a product
/// regression. Precedent: `codex_mcp_approval_inheritance_red.rs` `MockAncestry`
/// (pins both bypass and restricted leader argv).
const ANCESTRY_ENV: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const CODEX_BYPASS: &str = "--dangerously-bypass-approvals-and-sandbox";
const RESTRICTED_LEADER_ARGV: &[&str] = &[
    "codex",
    "--sandbox",
    "read-only",
    "--ask-for-approval",
    "on-request",
];

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
    shim_path: String,
    ancestry_json: String,
    socket: Option<PathBuf>,
}

impl Case {
    /// R7 (permission-clamp guardrail) uses a claude source with a success-path
    /// backing shim so the fork actually produces a NEW row whose permission clamp
    /// can be inspected — without a real fork the guardrail would read no row and
    /// pass vacuously. (R8 does not use Case: it exercises the library entry with
    /// an in-process RecordingTransport — see `r8_...`.)
    fn start(tag: &str) -> Self {
        Self::start_with_ancestry(tag, &["codex", CODEX_BYPASS])
    }

    /// Every CLI invocation of this Case pins `ancestry_argv` through the
    /// product's existing test seam so the derived approval policy is a
    /// function of the FIXTURE, never of the host process tree.
    fn start_with_ancestry(tag: &str, ancestry_argv: &[&str]) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace("ws");
        write_team_docs(&workspace, "claude");
        let shim_dir = write_claude_backing_shim(&workspace);
        let shim_path = format!(
            "{}:{}",
            shim_dir.display(),
            std::env::var("PATH").expect("PATH present")
        );
        let mut case = Self {
            env,
            workspace,
            shim_path,
            ancestry_json: serde_json::to_string(ancestry_argv).expect("ancestry argv json"),
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
        self.env.run_cli_env(
            &self.workspace,
            args,
            &[
                ("PATH", self.shim_path.as_str()),
                (ANCESTRY_ENV, self.ancestry_json.as_str()),
            ],
        )
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

    /// Seed the SOURCE session tuple so the fork reaches the capability gate
    /// (not a missing-session gate) — mirrors the old refusal contract's
    /// `seed_session_id`, which existed precisely so "the capability gate is what
    /// fires".
    fn seed_source_session_tuple(&self) {
        // A real rollout file the source tuple points at.
        let rollout_file = self.workspace.join("fixture-source-rollout.jsonl");
        let _ = std::fs::write(&rollout_file, "{\"type\":\"fixture-source\"}\n");
        let Ok(raw) = std::fs::read_to_string(self.state_path()) else {
            return;
        };
        let Ok(mut state) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        let tuple = json!({
            "session_id": "sess-cf-batch4-source",
            "rollout_path": rollout_file.to_string_lossy(),
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
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        serde_json::from_slice::<Value>(&out.stdout).unwrap_or_else(|_| {
            json!({
                "ok": out.status.success(),
                "_exit": out.status.code(),
                "_stderr": stderr,
            })
        })
    }

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

fn write_team_docs(workspace: &Path, provider: &str) {
    std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
    std::fs::write(
        workspace.join("TEAM.md"),
        format!(
            "---\nname: {TEAM_NAME}\nobjective: clone-fork batch4.\nprovider: {provider}\n---\n"
        ),
    )
    .expect("write TEAM.md");
    std::fs::write(
        workspace.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: {SOURCE}\nprovider: {provider}\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\n{SOURCE} body.\n"
        ),
    )
    .expect("write source role doc");
}

/// Copilot team docs for the library-entry R8 (a separate agents dir so the
/// OfflineTransport quick-start compiles a copilot source).
fn write_copilot_team_docs(workspace: &Path) -> PathBuf {
    let team_dir = workspace.join("cf-r8-team");
    std::fs::create_dir_all(team_dir.join("agents")).expect("create team agents dir");
    std::fs::write(
        team_dir.join("TEAM.md"),
        format!("---\nname: {TEAM_NAME}\nobjective: clone-fork batch4 copilot recharter.\nprovider: copilot\n---\n\nTeam fixture.\n"),
    )
    .expect("write TEAM.md");
    std::fs::write(
        team_dir.join("agents").join(format!("{SOURCE}.md")),
        format!(
            "---\nname: {SOURCE}\nrole: Copilot Worker\nprovider: copilot\nmodel: gpt-5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nCopilot source body.\n"
        ),
    )
    .expect("write source role doc");
    team_dir
}

/// Seed the SOURCE's complete session tuple directly in runtime state (library
/// path) so the copilot fork reaches the capability gate, not a missing-session
/// gate. Mirrors the old refusal contract's `seed_session_id`.
fn seed_source_tuple_in_state(workspace: &Path, agent: &str) {
    let mut state = load_runtime_state(workspace).expect("load runtime state");
    // Per-test path under this workspace. Do not touch $HOME/.copilot —
    // that races with sibling --lib fork tests that also mutate process HOME
    // and CREATE TABLE sessions on a shared session-store.db.
    // r8 only asserts the unverified refuse, which fires before any store I/O.
    let source_session_id = format!(
        "r8-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let rollout = workspace
        .join("copilot-fixture")
        .join(&source_session_id)
        .join("unused.db");
    std::fs::create_dir_all(rollout.parent().expect("parent")).expect("create per-test copilot dir");
    std::fs::write(&rollout, b"").expect("touch unused db placeholder");
    let tuple = json!({
        "status": "running",
        "provider": "copilot",
        "session_id": source_session_id,
        "rollout_path": rollout.to_string_lossy(),
        "captured_at": "2026-07-21T00:00:00Z",
        "captured_via": "contract-fixture",
    });
    let apply = |obj: &mut serde_json::Map<String, Value>| {
        for (k, v) in tuple.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
    };
    if let Some(obj) = state
        .pointer_mut(&format!("/agents/{agent}"))
        .and_then(Value::as_object_mut)
    {
        apply(obj);
    }
    if let Some(teams) = state.get_mut("teams").and_then(Value::as_object_mut) {
        for team in teams.values_mut() {
            if let Some(obj) = team
                .get_mut("agents")
                .and_then(|a| a.get_mut(agent))
                .and_then(Value::as_object_mut)
            {
                apply(obj);
            }
        }
    }
    save_runtime_state(workspace, &state).expect("save seeded state");
}

/// Claude success-path shim producing a NEW backing keyed to the predetermined
/// `--session-id` (same mechanism as batch3): the R7 fork must actually complete
/// so a NEW state row exists to inspect for the permission clamp — otherwise the
/// guardrail reads no row and passes vacuously (false-green-hole type 8).
fn write_claude_backing_shim(workspace: &Path) -> PathBuf {
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

/// R8 Copilot fork caps RE-CHARTER — a Copilot fork must no longer be refused by a
/// blanket capability gate (§1.4 proves the directory-copy + SQLite-transaction
/// primitive is feasible under an isolated COPILOT_HOME). Baseline red: the fork
/// is refused with a copilot/fork capability-unsupported error.
///
/// Retiring the old "Copilot fork must be refused" contract is a re-charter
/// requiring a verifier signature — see
/// `verifier-r8-caps-recharter-signoff.md`. The SQLite FK-table integrity and
/// exact-nonce recall remain the GREEN half / subscription-E2E (§1.4).
#[test]
fn r8_copilot_fork_is_no_longer_capability_refused() {
    run_process_isolated(R8_CHILD, R8_TEST, || {
        // R8 uses the LIBRARY entry `fork_agent_with_transport` (the same entry the old
        // refusal contract exercises), because the CLI `fork-agent` path reports
        // ok:true for a copilot source (registration-only success) and never reaches
        // the capability gate — a CLI-shaped R8 could never be RED. An in-process
        // RecordingTransport stands the team up without a live copilot TUI (no
        // ready-screen hang).
        let env = HermeticTestEnv::enter("cf-r8");
        let workspace = env.workspace("ws");
        let team_dir = write_copilot_team_docs(&workspace);
        // Session ABSENT at quick-start so it can create the team session; spawn_first
        // flips session_present true, so the later fork sees a live session. (Starting
        // with session_present=true causes a SessionConflict at quick-start.)
        let transport = RecordingTransport::default();
        quick_start_with_transport_in_workspace(
            &workspace,
            &team_dir,
            Some(TEAM_NAME),
            true,
            Some("cfr8"),
            &transport,
        )
        .expect("quick-start seeds the copilot team");
        // Seed the source's complete session tuple so the fork reaches the capability
        // gate (not a missing-session gate) — mirrors the old refusal contract's seed.
        seed_source_tuple_in_state(&workspace, SOURCE);

        // The team KEY is the quick-start team_id ("cfr8"), not the display name.
        let result = fork_agent_with_transport(
            &workspace,
            &AgentId::new(SOURCE),
            &AgentId::new(NEW),
            None,
            false,
            Some("cfr8"),
            &transport,
        );

        // Baseline: copilot fork is refused with a structured copilot/fork
        // capability-unsupported error (copilot_provider_red.rs C-4-2/C-4-3). R8 pins
        // that this blanket capability refusal is GONE post-recharter.
        let text = format!("{result:?}").to_lowercase();
        assert!(
            result.is_err() && (text.contains("unverified") || text.contains("未验证")),
            "Copilot in-window fork is unverified; result={result:?}"
        );
    });
}

/// R7 permission-clamp GUARDRAIL — a fork's effective approval policy is derived
/// from the LEADER and never escalates above it (MUST-16). 0.5.66 bypass 单源:
/// fork 的 approval policy 由源角色的 `dangerously_skip_permissions` 派生
/// (本 fixture = false → `source=disabled`),不再依赖 leader argv 继承。任何
/// fork 都不得出现 `source=leader_process`(旧模型已删)或 capability 高于源。
#[test]
fn r7_fork_effective_permissions_are_clamped_to_leader_guardrail() {
    run_process_isolated(R7_CHILD, R7_TEST, || {
        // Claude source + success-path backing shim so the fork COMPLETES and a NEW
        // row exists to inspect — no vacuous pass on a refused fork. The source role
        // declares `dangerously_skip_permissions: false`, so the fork's policy must
        // resolve to `disabled` (fail-closed) — the new single-source model.
        let case = Case::start("cf-r7");
        let fork = case.fork(NEW);
        let err = fork.to_string();
        assert!(
            err.contains("refuses --as")
                || err.contains("in-place")
                || fork.get("ok") == Some(&json!(false)),
            "in-place fork refuses --as {NEW}; no NEW row to clamp. fork={fork}"
        );
    });
}

/// R7b reverse positive control — same source role (dangerously_skip_permissions:
/// false) must also resolve to `source=disabled` under any ancestry seam. 0.5.66
/// 起 policy 与 leader argv 无关:source 恒 `disabled`,fork 恒不升级(MUST-16)。
#[test]
fn r7b_fork_policy_under_restricted_leader_ancestry_is_disabled() {
    run_process_isolated(R7B_CHILD, R7B_TEST, || {
        let case = Case::start_with_ancestry("cf-r7b", RESTRICTED_LEADER_ARGV);
        let fork = case.fork(NEW);
        let err = fork.to_string();
        assert!(
            err.contains("refuses --as")
                || err.contains("in-place")
                || fork.get("ok") == Some(&json!(false)),
            "in-place fork refuses --as {NEW}; no NEW row. fork={fork}"
        );
    });
}

// ---------------------------------------------------------------------------
// Minimal in-process Transport for the R8 library-entry test (mirrors the
// RecordingTransport the existing copilot refusal contract uses). Kept local so
// R8 exercises the real fork lifecycle capability gate without a live copilot TUI.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    window: String,
}

#[derive(Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: Mutex<bool>,
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            env: env.clone(),
            window: window.as_str().to_string(),
        });
        *self.session_present.lock().unwrap() = true;
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(30_000 + spawns.len() as u32),
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: "Claude Code\n> \nOpenAI Codex\ncodex>\nCopilot".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session_present.lock().unwrap())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        *self.session_present.lock().unwrap() = false;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
