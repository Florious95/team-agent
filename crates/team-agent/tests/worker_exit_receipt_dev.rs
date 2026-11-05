use std::collections::BTreeMap;
use std::path::Path;
#[path = "support/direct_fixture.rs"]
mod direct_fixture;

#[test]
fn exited_child_keeps_pane_alive_but_refuses_physical_delivery() {
    use serde_json::json;
    let f = direct_fixture::DirectFixture::new();
    let tmux = |args: &[&str]| {
        let output = std::process::Command::new("tmux").args(["-S", &f.socket]).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    };
    let script = team_agent::tmux_backend::worker_shell_wrapper_command(
        &["sh".into(), "-c".into(), "exit 7".into()], &f.root, &BTreeMap::new(), &[], "test",
    );
    tmux(&["respawn-pane", "-k", "-t", &f.pane, &script]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let exit = tmux(&["display-message", "-p", "-t", &f.pane, "#{@team-agent-provider-exit-status}"]);
        if exit.trim() == "7" { break; }
        assert!(std::time::Instant::now() < deadline, "wrapper never published exit: {exit}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(tmux(&["display-message", "-p", "-t", &f.pane, "#{pane_dead}"]).trim(), "0");
    f.set_state(&json!({"active_team_key":"current","teams":{"current":{
        "team_id":"current","tmux_socket":f.socket,"agents":{"worker-1":{
            "status":"running","provider":"retired-provider","pane_id":f.pane
        }}
    }}}));
    let value = team_agent::station::send::execute(team_agent::station::send::Request {
        workspace:&f.root,team:Some("current"),to:"worker-1",content:"must-not-swallow",task_id:None,message_id:None,
        presentation:&team_agent::messaging::presentation::PresentationRequest::default(),mailbox:false,envelope:None,
    },team_agent::station::send::Authority::Operator {sender:"leader"}).unwrap();
    assert_eq!(value["stored"],true,"{value}");
    assert_eq!(value["live"],false,"{value}");
    assert!(value["delivery_error"].as_str().unwrap().contains("recipient_provider_exited"),"{value}");
}

#[test]
fn worker_wrapper_publishes_exit_before_inert_tail_and_clears_old_exit() {
    let script = team_agent::tmux_backend::worker_shell_wrapper_command(
        &["codex".into()], Path::new("/tmp"), &BTreeMap::new(), &[], "codex",
    );
    let option = "@team-agent-provider-exit-status";
    assert_eq!(script.matches(option).count(), 2);
    assert!(script.find(&format!("{option} running")).unwrap() < script.find("codex").unwrap());
    assert!(script.find(&format!("{option} \"$rc\"")).unwrap() < script.find("exec /bin/sh").unwrap());
    assert!(script.contains("\"${TMUX%%,*}\""));
}
