#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn tmp_ws(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-init-removed-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn assert_removed(ws: &Path, flags: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["init", "--workspace", ws.to_str().unwrap()])
        .args(flags)
        .current_dir(ws)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid choice: 'init'"), "{stderr}");
}

#[test]
fn removed_init_never_creates_workspace_files_even_with_help_or_force() {
    let ws = tmp_ws("empty");
    for flags in [
        &[][..],
        &["--json"][..],
        &["--help"][..],
        &["-h"][..],
        &["--force", "--json"][..],
    ] {
        assert_removed(&ws, flags);
        assert_eq!(std::fs::read_dir(&ws).unwrap().count(), 0);
    }
    std::fs::remove_dir_all(ws).unwrap();
}

#[test]
fn removed_init_force_does_not_overwrite_existing_files() {
    let ws = tmp_ws("existing");
    let spec = ws.join(".team/current/team.spec.yaml");
    let state = ws.join("team_state.md");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    std::fs::write(&spec, "CUSTOM_SPEC\n").unwrap();
    std::fs::write(&state, "CUSTOM_STATE\n").unwrap();
    assert_removed(&ws, &["--force", "--json"]);
    assert_eq!(std::fs::read_to_string(spec).unwrap(), "CUSTOM_SPEC\n");
    assert_eq!(std::fs::read_to_string(state).unwrap(), "CUSTOM_STATE\n");
    assert!(!ws.join(".team/logs").exists());
    assert!(!ws.join(".team/runtime").exists());
    assert!(!ws.join("TEAM.md").exists());
    assert!(!ws.join("agents").exists());
    std::fs::remove_dir_all(ws).unwrap();
}
