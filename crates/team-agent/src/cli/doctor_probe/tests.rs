use super::*;

#[test]
fn failed_seat_remediation_never_restarts_healthy_siblings() {
    let action = restart_action(Path::new("/project path"), "X 运维", Some("worker-one"));
    assert_eq!(action.argv, ["team-agent", "restart", "worker-one", "--workspace", "/project path", "--team", "X 运维", "--json"]);
    assert!(!action.display().contains("worker-two"));
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "doctor-unit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn args(workspace: PathBuf) -> DoctorArgs {
    DoctorArgs {
        spec: None,
        workspace,
        gate: None,
        comms: false,
        team: None,
        fix: false,
        fix_schema: false,
        cleanup_orphans: false,
        confirm: false,
        json: true,
    }
}
fn output(result: CmdResult) -> String {
    crate::cli::emit::emit(&result.output, result.as_json).expect("rendered doctor output")
}

#[test]
fn scope_encoding_is_injective_and_rejects_traversal() {
    use crate::model::paths::{decode_scope_component, encode_scope_component};
    let names = [
        "X 运维",
        "X_运维",
        "x 运维",
        "X%20运维",
        "远程Agent安卓",
        "é",
        "e\u{301}",
        "space station 🚀",
        "legacy-team_1.v2",
    ];
    let mut paths = std::collections::BTreeSet::new();
    for name in names {
        let encoded = encode_scope_component(name).unwrap();
        assert!(encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-%".contains(&b)));
        assert_eq!(decode_scope_component(&encoded).unwrap(), name);
        assert!(
            paths.insert(encoded.to_ascii_lowercase()),
            "filesystem alias: {name}"
        );
    }
    assert_eq!(
        encode_scope_component("legacy-team_1.v2").unwrap(),
        "legacy-team_1.v2"
    );
    for name in [
        "",
        " ",
        ".",
        "..",
        "../other",
        "/tmp/other",
        "a\\b",
        "a\0b",
        "a\nb",
    ] {
        assert!(encode_scope_component(name).is_err(), "accepted {name:?}");
    }
    for path in ["%", "%2", "%GG", "%2F", "%41", "%FF"] {
        assert!(decode_scope_component(path).is_err());
    }
}

#[test]
fn utf8_inbox_and_seat_projection_preserve_verbatim_scope() {
    let scratch = Scratch::new();
    for team in ["X 运维", "X_运维", "x 运维", "X%20运维"] {
        crate::inbox_store::store(&scratch.0, "leader", &json!({"team":team,"content":team}))
            .unwrap();
        assert_eq!(
            crate::inbox_store::read(&scratch.0, "leader", 5, Some(team)).unwrap()["messages"][0]
                ["team"],
            team
        );
        let mut state =
            json!({"team_key":team,"agents":{"worker":{"provider":"fake","status":"stopped"}}});
        crate::station::projection::publish(&scratch.0, &mut state, team, &["worker"]).unwrap();
        assert_eq!(
            crate::station::projection::target(&scratch.0, team, "worker")
                .unwrap()
                .unwrap()
                .scope
                .team,
            team
        );
    }
    let view = crate::station::projection::read_workspace(&scratch.0)
        .unwrap()
        .unwrap();
    assert_eq!(view["teams"].as_object().unwrap().len(), 4);
    for team in ["X 运维", "X_运维", "x 运维", "X%20运维"] {
        assert_eq!(view["teams"][team]["team_key"], team);
    }
}

#[test]
fn storage_failure_is_not_relabelled_as_missing_leader() {
    let scratch = Scratch::new();
    fs::create_dir_all(scratch.0.join(".team/runtime")).unwrap();
    fs::write(scratch.0.join(".team/runtime/state.json"), serde_json::to_vec(&json!({
        "active_team_key":"probe", "teams":{"probe":{"team_key":"probe","agents":{},
        "leader_receiver":{"pane_id":"%99","provider":"fake","tmux_socket":"owned-absent-host"}}}
    })).unwrap()).unwrap();
    fs::write(scratch.0.join(".team/inbox"), "not a directory").unwrap();
    let request = crate::cli::SendArgs {
        target: Some("leader".into()),
        message: vec!["must not send".into()],
        targets: None,
        workspace: scratch.0.clone(),
        team: None,
        task: None,
        sender: crate::messaging::TrustedSender::leader(),
        no_ack: false,
        no_wait: true,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: true,
        message_id: None,
        presentation: Default::default(),
        pane: None,
        to_name: None,
        to_leader: None,
    };
    let error = crate::cli::cmd_send(&request).unwrap_err();
    let crate::cli::CliError::Runtime(cause) = error else {
        panic!("storage cause replaced: {error:?}")
    };
    assert!(!cause.contains("not present in any team"));
    assert!(
        cause.contains("directory") || cause.contains("os error"),
        "{cause}"
    );
}

#[test]
fn default_scope_recovers_only_the_unique_team() {
    let scratch = Scratch::new();
    fs::create_dir_all(scratch.0.join(".team/runtime")).unwrap();
    let path = scratch.0.join(".team/runtime/state.json");
    let mut state =
        json!({"active_team_key":"current","teams":{"X 运维":{"team_key":"X 运维","agents":{}}}});
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        crate::direct_delivery::directory(&scratch.0, None)
            .unwrap()
            .team
            .as_deref(),
        Some("X 运维")
    );
    assert!(crate::direct_delivery::directory(&scratch.0, Some("current")).is_err());
    state["teams"]["other"] = json!({"team_key":"other","agents":{}});
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(crate::direct_delivery::directory(&scratch.0, None).is_err());
    state["active_team_key"] = json!("X 运维");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        crate::direct_delivery::directory(&scratch.0, None)
            .unwrap()
            .team
            .as_deref(),
        Some("X 运维")
    );
}

#[test]
fn inbox_probe_distinguishes_checksum_failure_from_uncommitted_tail() {
    let scratch = Scratch::new();
    crate::inbox_store::store(
        &scratch.0,
        "leader",
        &json!({"team":"probe","content":"good"}),
    )
    .unwrap();
    let path = scratch.0.join(".team/inbox/probe/leader/00000001.jsonl");
    let original = fs::read_to_string(&path).unwrap();
    assert_eq!(
        crate::inbox_store::probe_tail(&scratch.0, "leader", Some("probe")).unwrap(),
        (1, false)
    );
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{partial")
        .unwrap();
    assert_eq!(
        crate::inbox_store::probe_tail(&scratch.0, "leader", Some("probe")).unwrap(),
        (1, true)
    );
    fs::write(&path, original.replace("good", "evil")).unwrap();
    assert!(
        crate::inbox_store::probe_tail(&scratch.0, "leader", Some("probe"))
            .unwrap_err()
            .contains("inbox_frame_invalid")
    );
}

#[test]
fn storage_probe_cleans_its_artifacts() {
    let scratch = Scratch::new();
    let agents = serde_json::Map::new();
    assert!(filesystem_probe(&scratch.0, "probe", &agents)
        .unwrap()
        .contains("write/read/rename/delete"));
    assert_eq!(
        fs::read_dir(scratch.0.join(".team/runtime/.doctor"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
#[cfg(unix)]
fn contended_runtime_lock_is_inconclusive_not_deadlock() {
    let scratch = Scratch::new();
    fs::create_dir_all(scratch.0.join(".team/runtime")).unwrap();
    let path = scratch.0.join(".team/runtime/state-save.lock");
    let lock = File::create(&path).unwrap();
    assert!(crate::platform::file_lock::try_lock_once_nonblocking(&lock).unwrap());
    let error = filesystem_probe(&scratch.0, "probe", &serde_json::Map::new()).unwrap_err();
    assert!(error.starts_with("INCONCLUSIVE:"));
    assert!(path.exists());
    assert_eq!(
        fs::read_dir(scratch.0.join(".team/runtime/.doctor"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn rendering_is_bounded_and_not_a_round_trip_claim() {
    let links: Vec<_> = (1..=6)
        .map(|n| {
            Link::new(
                n,
                "PASS",
                if n == 6 {
                    "leader %0@/owned/leader.sock reachable"
                } else {
                    "checked"
                },
            )
        })
        .collect();
    let human = output(render(
        links.clone(),
        Path::new("/owned/workspace"),
        "probe",
        json!({}),
        (1, 1),
        None,
        "",
        false,
        Instant::now(),
    ));
    assert_eq!(human.lines().count(), 1);
    assert!(human.contains("all 6 links passed"));
    assert!(human.contains("%0@/owned/leader.sock") && human.ends_with("s]"));
    let machine = output(render(
        links,
        Path::new("/owned/workspace"),
        "probe",
        json!({}),
        (1, 1),
        None,
        "",
        true,
        Instant::now(),
    ));
    assert!(machine.len() < 2000);
    let value: Value = serde_json::from_str(&machine).unwrap();
    assert_eq!(value["links"][5]["link"], "return");
    assert_eq!(value["links"][5]["proof"], "probed");
    assert_eq!(value["links"][3]["proof"], "exercised");
    assert!(value["links"]
        .as_array()
        .unwrap()
        .iter()
        .all(|link| matches!(link["proof"].as_str(), Some("probed" | "exercised"))));
    assert!(value["links"][5]["message"]
        .as_str()
        .unwrap()
        .contains("%0@/owned/leader.sock"));
    assert_eq!(
        serde_json::from_str::<Value>(&machine).unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    for banned in ["coordinator", "providers", "details:"] {
        assert!(!machine.contains(banned));
    }
}

#[test]
fn caller_scope_does_not_depend_on_workers_or_receiver_metadata() {
    let caller = json!({"socket":"/owned/live.sock","pane":"%0","session":"live-session"});
    let mut raw = json!({"teams":{
        "dead":{"tmux_socket":"/owned/dead.sock","session_name":"dead","agents":{}},
        "live":{"tmux_socket":"/owned/live.sock","session_name":"live-session","agents":{}}
    }});
    assert_eq!(caller_team(&raw, &caller).as_deref(), Some("live"));
    raw["teams"]["live"]["agents"]["worker"] =
        json!({"status":"running","pane_id":"%0","native_endpoint":"/owned/live.sock"});
    assert_eq!(
        caller_team(&raw, &caller),
        None,
        "an active worker is not a leader context"
    );
    raw["teams"]["live"]["agents"]["worker"]["native_endpoint"] = json!("/owned/dead.sock");
    assert_eq!(
        caller_team(&raw, &caller).as_deref(),
        Some("live"),
        "pane IDs are socket-qualified"
    );
    raw["teams"]["other"] = raw["teams"]["live"].clone();
    assert_eq!(
        caller_team(&raw, &caller),
        None,
        "ambiguous caller anchors must not pick an arbitrary team"
    );
}

#[test]
fn claim_remedy_preserves_argv_and_execution_context_without_force() {
    let remedy = action("claim-leader", Path::new("/owned/X 运维"), "X 运维");
    let value = serde_json::to_value(&remedy).unwrap();
    assert_eq!(
        value["argv"],
        json!([
            "team-agent",
            "claim-leader",
            "--workspace",
            "/owned/X 运维",
            "--team",
            "X 运维",
            "--confirm",
            "--json"
        ])
    );
    assert_eq!(value["context"], "from the current leader terminal");
    assert!(remedy.display().contains("--workspace '/owned/X 运维'"));
    assert!(!remedy.argv.iter().any(|arg| arg == "--force"));
}

#[test]
fn claim_fix_is_copyable_and_context_is_separate() {
    let links = (1..=6)
        .map(|n| Link::new(n, if n == 6 { "FAIL" } else { "PASS" }, "checked"))
        .collect();
    let remedy = action("claim-leader", Path::new("/owned/X 运维"), "X 运维");
    let expected = format!("fix: {}", remedy.display());
    let human = output(render(
        links,
        Path::new("/owned/X 运维"),
        "X 运维",
        json!({}),
        (1, 1),
        Some(remedy),
        "no forced takeover",
        false,
        Instant::now(),
    ));
    assert!(human.lines().count() <= 10);
    assert!(human.lines().any(|line| line == expected));
    assert!(human
        .lines()
        .any(|line| line == "context: from the current leader terminal"));
    assert!(expected.starts_with("fix: team-agent claim-leader "));
    assert!(!expected.contains("from the current leader terminal"));
}

#[test]
fn invalid_scope_blocks_without_creating_workspace() {
    let scratch = Scratch::new();
    let missing = scratch.0.join("absent");
    let mut request = args(missing.clone());
    let machine: Value = serde_json::from_str(&output(run(&request))).unwrap();
    assert_eq!(machine["links"][0]["status"], "FAIL");
    assert!(machine["links"].as_array().unwrap()[1..]
        .iter()
        .all(|link| link["status"] == "BLOCKED"));
    request.json = false;
    assert!(output(run(&request)).lines().count() <= 5);
    assert!(!missing.exists());
}

#[test]
#[cfg(unix)]
fn native_probe_deadline_includes_wrapper_children_and_output_pipes() {
    let start = Instant::now();
    let error = crate::tmux_backend::RealCommandRunner
        .run_with_budget(
            &["sh".into(), "-c".into(), "sleep 4".into()],
            Duration::from_millis(100),
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(1));

    let scratch = Scratch::new();
    let delayed_effect = scratch.0.join("must-not-appear");
    let command = format!(
        "(sleep 0.3; printf done > '{}') & exit 0",
        delayed_effect.display()
    );
    let error = crate::tmux_backend::RealCommandRunner
        .run_with_budget(
            &["sh".into(), "-c".into(), command],
            Duration::from_millis(100),
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        !delayed_effect.exists(),
        "timed-out wrapper descendant survived"
    );
}
