//! SMOKE-1 (locate .team/artifacts/smoke1-doctor-duplicate-locate.md):
//!
//! `team-agent doctor` run from a HOME that contains a legacy `~/.team/current` team
//! (left over from 0.2.x / earlier 0.3.x installs) must not bring the whole doctor
//! down with `error: "profile_smoke_failed"` simply because the legacy team's role
//! docs no longer compile. A fresh-install self-check is a global concern; the
//! legacy team is real user state but it is not the doctor's target.
//!
//! Deterministic RED:
//!   1. Build a temp HOME with `~/.team/current/{TEAM.md, agents/a.md, agents/b.md}`.
//!   2. Both `agents/a.md` and `agents/b.md` use the same `name: worker` (so
//!      `compile_team` raises "/agents: duplicate agent id" via spec.rs::semantic_errors).
//!   3. Invoke `team-agent doctor --workspace <home> --json`.
//!
//! Expected after fix:
//!   * The doctor JSON must NOT carry the top-level `error: "profile_smoke_failed"`
//!     produced by implicitly compiling `<home>/.team/current`.
//!   * `profile_smoke.status` must NOT be `profile_invalid` from the implicit
//!     legacy target — the only paths now allowed for implicit doctor are:
//!       (a) `not_required`(legacy `.team/current` skipped — recommended thin rule);
//!       (b) `legacy_team_invalid`(degraded diagnostic that surfaces the team_dir
//!           and an actionable next step instead of the bare profile_smoke fail).
//!   * If the compile failure is surfaced in some form, the message must contain
//!     enough to act on (the legacy team_dir path) — bare "/agents: duplicate
//!     agent id" alone violates the N38-style failure explainability anchor.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

const TEAM_MD: &str =
    "---\nname: legacy-team\nobjective: SMOKE-1 legacy team fixture.\nprovider: codex\n---\n\nteam.\n";

const ROLE_A: &str = "---\nname: worker\nrole: Worker\nprovider: codex\nmodel: gpt-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker A role doc.\n";

const ROLE_B: &str = "---\nname: worker\nrole: Worker\nprovider: codex\nmodel: gpt-5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker B role doc (duplicate id).\n";

#[test]
fn doctor_does_not_fail_solely_because_legacy_home_dot_team_current_is_invalid() {
    let home = tmp_dir("home-legacy");
    let legacy = home.join(".team").join("current");
    std::fs::create_dir_all(legacy.join("agents")).expect("mkdir legacy agents");
    std::fs::write(legacy.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(legacy.join("agents").join("a.md"), ROLE_A).expect("write agents/a.md");
    std::fs::write(legacy.join("agents").join("b.md"), ROLE_B).expect("write agents/b.md");

    let output = run(
        &["doctor", "--workspace", home.to_str().unwrap(), "--json"],
        &home,
        &home,
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor --json must emit parseable JSON; parse_error={error}; stdout={} stderr={}",
            stdout(&output),
            stderr(&output)
        )
    });

    // C-1: doctor must not surface the implicit legacy `.team/current` compile
    // failure as the top-level `profile_smoke_failed` error. A bare bare-id
    // "/agents: duplicate agent id" string from the legacy team is not actionable
    // for a fresh installer self-check.
    let top_error = body.get("error").cloned().unwrap_or(Value::Null);
    assert!(
        top_error != Value::String("profile_smoke_failed".to_string()),
        "doctor must not return top-level error=profile_smoke_failed when the only \
         compile failure is the legacy <home>/.team/current; body={body}"
    );

    // C-2: profile_smoke for the implicit doctor invocation cannot land as
    // `profile_invalid` purely from the legacy team — it must either be skipped
    // (`not_required`) or surfaced as a degraded `legacy_team_invalid` shape that
    // names the legacy team dir + actionable next step.
    let smoke = body
        .get("profile_smoke")
        .cloned()
        .expect("doctor JSON must include profile_smoke");
    let smoke_status = smoke.get("status").and_then(Value::as_str).unwrap_or("");
    let is_skipped = smoke_status == "not_required";
    let is_legacy = smoke_status == "legacy_team_invalid";
    assert!(
        is_skipped || is_legacy,
        "profile_smoke.status for implicit legacy <home>/.team/current must be \
         `not_required` (skipped) or `legacy_team_invalid` (actionable degraded); \
         got status={smoke_status:?} smoke={smoke} body={body}"
    );

    // C-3: if the degraded `legacy_team_invalid` shape is used, it must name the
    // legacy team_dir so the user can act (open it, remove it, or run a scoped
    // `team-agent doctor <team-dir>` against it).
    if is_legacy {
        let team_dir = smoke.get("team_dir").and_then(Value::as_str).unwrap_or("");
        let expected = legacy.to_string_lossy().to_string();
        assert!(
            team_dir.contains(".team/current") || team_dir == expected,
            "legacy_team_invalid profile_smoke must name the legacy team_dir \
             (<home>/.team/current); got team_dir={team_dir:?} expected~={expected:?} smoke={smoke}"
        );
    }
}

#[test]
fn spec_duplicate_agent_id_validation_names_the_duplicated_id() {
    // Locate doc §"Smallest likely code touch" item 3: bare "/agents: duplicate
    // agent id" is not actionable. The validation error must contain enough to
    // name the duplicated id ("worker" in this fixture) so an operator can find
    // the conflicting role docs without spelunking.
    use team_agent::compiler::compile_team;
    let team = tmp_dir("compile-dup");
    std::fs::create_dir_all(team.join("agents")).expect("mkdir agents");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("a.md"), ROLE_A).expect("write a.md");
    std::fs::write(team.join("agents").join("b.md"), ROLE_B).expect("write b.md");

    let err = compile_team(&team)
        .err()
        .expect("compile_team must error on duplicate agent id");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate agent id"),
        "compile_team error must mention duplicate agent id; got {msg}"
    );
    assert!(
        msg.contains("worker"),
        "compile_team duplicate-id error must name the duplicated id `worker` so an \
         operator can find the conflicting role docs; got {msg}"
    );
}

fn run(args: &[&str], cwd: &Path, home: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run team-agent doctor")
}

fn tmp_dir(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-smoke1-doctor-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}
