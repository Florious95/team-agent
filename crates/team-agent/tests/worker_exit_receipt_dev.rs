use std::collections::BTreeMap;
use std::path::Path;

#[test]
fn worker_wrapper_publishes_exit_before_inert_tail_and_clears_old_exit() {
    let script = team_agent::tmux_backend::worker_shell_wrapper_command(
        &["codex".into()], Path::new("/tmp"), &BTreeMap::new(), &[], "codex",
    );
    let option = "@team-agent-provider-exit-status";
    assert_eq!(script.matches(option).count(), 2);
    assert!(script.find(&format!("{option} running")).unwrap() < script.find("codex").unwrap());
    assert!(script.find(&format!("{option} \"$rc\"")).unwrap() < script.find("while sleep").unwrap());
    assert!(script.contains("\"${TMUX%%,*}\""));
}
