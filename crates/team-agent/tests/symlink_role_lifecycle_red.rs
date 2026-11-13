#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![cfg(unix)]
//! RED suite (ISSUE-3): a symlinked role document must survive the ordinary lifecycle.
//!
//! Users keep hand-written role templates elsewhere and link `agents/<id>.md` to them.
//! Case A: with no argument changes, stopping and starting such a seat again is a semantic no-op.
//!         It must succeed and must not write the user's template. Today the start normalizes
//!         the declaration and fails with `state persistence failed: refusing to replace symlink`.
//! Case B: a configuration change (`--model X`, optionally `--session fresh`) cannot be persisted
//!         through a symlink. It must be refused during planning (`--dry-run` already refuses) with
//!         guidance naming the symlink target to edit, and before any lifecycle side effect: no
//!         session archive, no state, role or spec write. Today the session is archived first.
//! The `g*` controls run the same flows on a regular role file and must stay green.
//!
//! Black-box: real CLI binary, real tmux, fake-worker panes, isolated HOME.

#[path = "e2e/framework.rs"]
mod framework;

use framework::{run_ta, TaResult, TestWorkspace};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

const SEAT: &str = "sym";
/// Hand-written, non-canonical: no name/role/bypass keys, so a normalized declaration differs.
const TEMPLATE: &str = "---\nprovider: fake\nmodel: fake\n---\nHand-written fake worker.\n";

fn log(step: &str, detail: impl std::fmt::Display) {
    eprintln!("[symlink-red] {step}: {detail}");
}

fn transcript(out: &TaResult) -> String {
    format!("argv={:?} exit={} stdout={} stderr={}", out.argv, out.exit_code, out.stdout, out.stderr)
}

fn ws_str(ws: &TestWorkspace) -> String {
    ws.path().to_str().unwrap().to_string()
}

fn ta(ws: &TestWorkspace, args: &[&str]) -> TaResult {
    let out = run_ta(ws, args);
    log("command", transcript(&out));
    own_sockets(ws);
    out
}

/// Every SUT-created `ta-*` tmux endpoint named in state, registered for cleanup.
fn own_sockets(ws: &TestWorkspace) {
    let Ok(raw) = fs::read_to_string(ws.state_json_path()) else { return };
    let state: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut found = vec![state["tmux_socket"].clone()];
    if let Some(teams) = state["teams"].as_object() {
        found.extend(teams.values().map(|team| team["tmux_socket"].clone()));
    }
    for socket in found.iter().filter_map(Value::as_str).map(PathBuf::from) {
        if socket.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ta-")) && socket.exists() {
            ws.register_owned_tmux_socket(&socket);
        }
    }
}

struct Fixture {
    ws: TestWorkspace,
    tag: String,
    link: PathBuf,
    target: PathBuf,
    linked: bool,
}

/// A team whose only worker's role is `TEMPLATE`: a symlink into `role-templates/` when `linked`,
/// otherwise a regular file (control). The team is created with `start --from` and is running.
fn team(tag: &str, linked: bool) -> Fixture {
    let ws = TestWorkspace::new(tag).with_fake_spec(&[SEAT]);
    let link = ws.path().join("agents").join(format!("{SEAT}.md"));
    let templates = ws.path().join("role-templates");
    fs::create_dir_all(&templates).unwrap();
    fs::write(templates.join(format!("{SEAT}.md")), TEMPLATE).unwrap();
    let target = templates.join(format!("{SEAT}.md")).canonicalize().unwrap();
    fs::remove_file(&link).unwrap();
    if linked {
        symlink(&target, &link).unwrap();
    } else {
        fs::write(&link, TEMPLATE).unwrap();
    }
    let fixture = Fixture { ws, tag: tag.to_string(), link, target, linked };
    let dir = ws_str(&fixture.ws);
    let created = ta(&fixture.ws, &["start", "--from", &dir, "--workspace", &dir, "--team", tag, "--json"]);
    assert_eq!(created.exit_code, 0, "fixture: start --from must create the team\n{}", transcript(&created));
    fixture.assert_role_untouched("after start --from");
    assert_eq!(fixture.runtime_status(), "running", "fixture: the worker must be running after start --from");
    fixture
}

impl Fixture {
    fn args<'a>(&'a self, dir: &'a str, head: &[&'a str]) -> Vec<&'a str> {
        let mut args = head.to_vec();
        args.extend_from_slice(&["--workspace", dir, "--team", &self.tag, "--json"]);
        args
    }

    fn run(&self, head: &[&str]) -> TaResult {
        let dir = ws_str(&self.ws);
        let args = self.args(&dir, head);
        ta(&self.ws, &args)
    }

    fn runtime_status(&self) -> String {
        let out = self.run(&["status"]);
        let value: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
        value["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|node| node["name"].as_str() == Some(SEAT)).cloned())
            .and_then(|node| node["runtime_status"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no status node for {SEAT}\n{}", transcript(&out)))
    }

    /// The user's template is never rewritten; a linked role stays the same link.
    fn assert_role_untouched(&self, step: &str) {
        if self.linked {
            let meta = fs::symlink_metadata(&self.link).unwrap();
            assert!(meta.file_type().is_symlink(), "{step}: {} must remain a symlink", self.link.display());
            assert_eq!(fs::read_link(&self.link).unwrap(), self.target, "{step}: the symlink must keep its target");
            assert_eq!(fs::read_to_string(&self.target).unwrap(), TEMPLATE, "{step}: the linked template must not be rewritten");
        }
    }

    /// Byte-exact evidence that no lifecycle side effect happened: the seat's state rows,
    /// the role link and target, and the runtime spec.
    fn side_effect_evidence(&self) -> String {
        let state = self.ws.read_state();
        let spec = state["spec_path"].as_str().map(|p| fs::read(p).unwrap_or_default()).unwrap_or_default();
        format!(
            "top={}\nteam={}\nlink={:?}\ntarget={:?}\nspec_len={}\nspec={}",
            state["agents"][SEAT],
            state["teams"][self.tag.as_str()]["agents"][SEAT],
            fs::read_link(&self.link).ok(),
            fs::read(&self.target).ok(),
            spec.len(),
            String::from_utf8_lossy(&spec)
        )
    }
}

fn assert_ok(out: &TaResult, step: &str) {
    assert!(out.exit_code == 0 && !format!("{}{}", out.stdout, out.stderr).contains("refusing to replace symlink"), "{step}\n{}", transcript(out));
}

/// Case A: stop + start (seat and team) with no argument changes.
fn semantic_noop_resume(tag: &str, linked: bool) {
    let fixture = team(tag, linked);
    let stopped = fixture.run(&["stop", SEAT]);
    assert_eq!(stopped.exit_code, 0, "stop must succeed\n{}", transcript(&stopped));
    let started = fixture.run(&["start", SEAT]);
    assert_ok(&started, "Case A: an unchanged start of a stopped seat with a hand-written role must succeed");
    assert_eq!(fixture.runtime_status(), "running", "Case A: the seat must be running again");
    fixture.assert_role_untouched("Case A: after the seat start");
    let team_stop = fixture.run(&["stop"]);
    assert_eq!(team_stop.exit_code, 0, "team stop must succeed\n{}", transcript(&team_stop));
    let team_start = fixture.run(&["start"]);
    assert_ok(&team_start, "Case A: an unchanged team start with a hand-written role must succeed");
    assert_eq!(fixture.runtime_status(), "running", "Case A: the seat must be running after the team start");
    fixture.assert_role_untouched("Case A: after the team start");
}

#[test]
fn r1_symlinked_handwritten_role_resumes_as_a_semantic_noop() {
    semantic_noop_resume("sym-r1", true);
}

#[test]
fn g1_control_regular_handwritten_role_resumes() {
    semantic_noop_resume("sym-g1", false);
}

/// Case B: a configuration change on a stopped seat whose role is a symlink.
#[test]
fn r2_symlinked_role_change_is_refused_in_planning_before_any_side_effect() {
    let fixture = team("sym-r2", true);
    let stopped = fixture.run(&["stop", SEAT]);
    assert_eq!(stopped.exit_code, 0, "stop must succeed\n{}", transcript(&stopped));
    let target = fixture.target.to_str().unwrap().to_string();
    let mut deficits = Vec::new();
    for extra in [&["--model", "other-model"][..], &["--model", "other-model", "--session", "fresh"][..]] {
        for dry in [true, false] {
            let mut head = vec!["start", SEAT];
            head.extend_from_slice(extra);
            if dry {
                head.push("--dry-run");
            }
            let before = fixture.side_effect_evidence();
            let out = fixture.run(&head);
            let text = format!("{}{}", out.stdout, out.stderr);
            let after = fixture.side_effect_evidence();
            let case = format!("{head:?}");
            if out.exit_code == 0 {
                deficits.push(format!("{case}: must be refused (a change cannot be persisted through the symlink)\n{}", transcript(&out)));
            } else if !(text.contains("symlink") && text.contains(&target)) {
                deficits.push(format!("{case}: the refusal must name the symlink target to edit ({target})\n{}", transcript(&out)));
            }
            if before != after {
                deficits.push(format!("{case}: refused change must have no side effect\n--- before ---\n{before}\n--- after ---\n{after}"));
            }
            fixture.assert_role_untouched(&case);
        }
    }
    assert_eq!(fixture.runtime_status(), "stopped", "Case B: the refused seat must stay stopped");
    assert!(deficits.is_empty(), "Case B deficits:\n{}", deficits.join("\n\n"));
}

/// Control for Case B: the same change on a regular role file is accepted.
#[test]
fn g2_control_regular_role_change_is_accepted() {
    let fixture = team("sym-g2", false);
    let stopped = fixture.run(&["stop", SEAT]);
    assert_eq!(stopped.exit_code, 0, "stop must succeed\n{}", transcript(&stopped));
    let out = fixture.run(&["start", SEAT, "--model", "other-model"]);
    assert_eq!(out.exit_code, 0, "a model change on a regular role file must be accepted\n{}", transcript(&out));
    assert!(fs::read_to_string(&fixture.link).unwrap().contains("other-model"), "the regular role file records the change");
}
