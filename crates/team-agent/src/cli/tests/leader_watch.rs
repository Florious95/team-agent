use super::*;

    // =========================================================================
    // (DELEGATION sweep, rt-host-a loop #2) — cmd_send persistence + shutdown teardown.
    // =========================================================================

    // 1 [P0, CONFIRMED BUG] — cmd_send (cli/send.rs) returns delivery_json synthetic (true,"delivered")
    // and NEVER calls messaging::send_message, so NO `messages` row is persisted. RED: after cmd_send to
    // a worker on a seeded ws (real MessageStore), assert a messages row was PERSISTED (query by
    // recipient+content) — proving the real messaging::send_message -> MessageStore.create_message path.
    // DB-persist is observable without a transport. (messaging::send_message is ALSO a stub today, so the
    // porter wires BOTH cmd_send->send_message AND send_message->persist.)
    //
    // OLD seed: flat `{"agents": {"w1": ...}}` worked because send_message read agents
    // directly off the raw runtime state.
    // NEW seed (Bug 1/2 — team-in-team state scope, see tests/team_in_team_state_scope_red.rs):
    //   cmd_send → resolve_active_team yields a team_key, send_message projects the
    //   raw state through `project_top_level_view(team_key)` which reads agents off
    //   `teams[team_key].agents`. The seed therefore lives under `teams.current.agents`
    //   with `active_team_key=current`; the delegation + persistence behavior under test
    //   is unchanged — only the shape of "an in-team recipient" is now nested.
    #[test]
    fn cli_send_persists_real_message_row() {
        let ws = deleg_uniq_dir("send");
        let _ = crate::message_store::MessageStore::open(&ws).unwrap(); // real store at the workspace
        // w1 must be a known team agent — golden send.py refuses non-team targets
        // (target_not_in_team); an in-team recipient is the one that persists. Bug 1/2
        // scopes agents under teams[<key>].agents (NEW shape).
        crate::state::persist::save_runtime_state(
            &ws,
            &serde_json::json!({
                "active_team_key": "current",
                "teams": {"current": {"agents": {"w1": {"provider": "codex"}}}}
            }),
        )
        .unwrap();
        let args = SendArgs {
            target: Some("w1".to_string()),
            message: vec!["hello-real-delegation".to_string()],
            targets: None,
            workspace: ws.clone(),
            team: None,
            task: None,
            sender: "leader".to_string(),
            no_ack: false,
            no_wait: true,
            watch_result: false,
            timeout: 0.0,
            confirm_human: false,
            json: true,
            message_id: None,
            pane: None,
            to_name: None,
        to_leader: None,
        };
        let _ = cmd_send(&args);

        let store = crate::message_store::MessageStore::open(&ws).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        let count: i64 = conn
            .query_row(
                "select count(*) from messages where recipient = 'w1' and content = 'hello-real-delegation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            count >= 1,
            "cmd_send must delegate to messaging::send_message and PERSIST a `messages` row (recipient=w1); \
             the synthetic delivery_json writes NO DB row -> count={count}"
        );
    }

    // 5 [P1, CONFIRMED PARTIAL] — shutdown stops the coordinator but NEVER kills the team tmux session
    // (-> orphan worker panes). #[ignore] real-machine: the wired shutdown kills the real tmux session.
    // SEAM NEEDED (note to porter): add shutdown_with_transport(workspace, keep_logs, team, &dyn Transport)
    // (mirror restart_with_transport) so this can assert IN-PROCESS that transport.kill_session(team
    // session) was called via a RecordingTransport — the clean "team session killed / workers reaped"
    // observable. Until that seam lands, this asserts the real teardown surfaces (session-kill / stop).
    #[test]
    #[ignore = "real-machine: shutdown kills the team tmux session. PORTER SEAM: add \
                shutdown_with_transport(workspace, keep_logs, team, &dyn Transport) so kill_session is \
                assertable in-process via a RecordingTransport (workers reaped, no orphan panes)."]
    fn cli_shutdown_kills_team_session_real_teardown() {
        let ws = seed_status_workspace(); // state.json with a running agent + session_name
        let args = ShutdownArgs { workspace: ws, team: None, keep_logs: true, json: true };
        let text = format!("{:?}", cmd_shutdown(&args)).to_lowercase();
        assert!(
            text.contains("session") || text.contains("killed") || text.contains("kill_session"),
            "shutdown must KILL the team tmux session + reap workers (real teardown); the rt-host-a partial \
             bug stops the coordinator but leaves the session + workers running (orphans); got {text}"
        );
    }

    // =========================================================================
    // WAVE-2 Lane B — CLI leader handler delegation byte-parity (leader_port::*).
    //   The three CLI verbs are thin pass-throughs (cli/commands.py:152-161):
    //     cmd_takeover     -> runtime.takeover(ws, team, confirm)
    //     cmd_claim_leader -> runtime.claim_leader(ws, team, confirm)  (Family A)
    //     cmd_identity     -> runtime.leader_identity(ws, team)
    //   leader_port::{takeover,claim_leader,leader_identity} are STUBS returning
    //   the WRONG shape today -> these LOCK the golden dict so the porter wires
    //   them into leader::* / runtime.* and matches byte-for-byte.
    //   Golden re-probed @ team-agent-public (probe_claim.py / probe_rtclaim.py /
    //   probe_lid.py). Label: RED = stub returns wrong shape today.
    // =========================================================================

    fn leader_port_ws(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ta-cli-leaderport-{}-{}",
            tag,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // #235 / I-RN-3 — explicit takeover is unconditional once the caller has a live pane:
    // it is not a permission gate and must replace a live owner, advancing owner_epoch.
    // `--confirm` may remain a UX affordance, but it is not the authority check.
    #[test]
    fn leader_port_takeover_refuses_without_confirm_byte_parity_obsolete_now_unconditional() {
        let cli = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/mod.rs"),
        )
        .unwrap();
        assert_eq!(
            cli.matches("claim_leader(workspace, team, true)").count(),
            1,
            "takeover must call the lease path as an explicit takeover, not pass the CLI confirm flag as a permission gate"
        );

        let ws = leader_port_ws("tk_unconditional_live");
        let team_id = crate::model::ids::TeamKey::new("current");
        let caller = crate::transport::PaneId::new("%5");
        let mut state = json!({
            "session_name": "team-agent-x",
            "team_owner": {
                "pane_id": "%1",
                "provider": "codex",
                "machine_fingerprint": "fp",
                "leader_session_uuid": "OWNERUUID",
                "owner_epoch": 2,
                "claimed_at": "t",
                "claimed_via": "claim-leader"
            },
            "leader_receiver": {
                "pane_id": "%1",
                "provider": "codex",
                "owner_epoch": 2,
                "leader_session_uuid": "OWNERUUID"
            }
        });
        let event_log = crate::event_log::EventLog::new(&ws);
        let live = LeaderPortSeededLiveness::new(&["%1", "%5"]);
        let r = crate::leader::claim_lease_no_incident(
            &ws,
            &mut state,
            None,
            &team_id,
            &caller,
            true,
            &event_log,
            &live,
        )
        .unwrap();

        assert!(r.ok, "explicit takeover must succeed even when old owner pane is live: {r:?}");
        assert_eq!(r.status, crate::leader::LeaseStatus::Claimed);
        assert_eq!(r.owner_epoch, Some(crate::model::ids::OwnerEpoch(3)));
        assert_eq!(r.bound_pane_id, Some(caller.clone()));
        // Stage 3d: canonical owner/receiver at teams.<team_key>.
        let team_key = crate::state::projection::team_state_key(&state);
        assert_eq!(state["teams"][&team_key]["leader_receiver"]["pane_id"], json!("%5"));
        assert_eq!(state["teams"][&team_key]["team_owner"]["pane_id"], json!("%5"));
        assert_eq!(state["teams"][&team_key]["team_owner"]["owner_epoch"], json!(3));
    }

    struct LeaderPortSeededLiveness {
        live_panes: std::collections::BTreeSet<String>,
    }

    impl LeaderPortSeededLiveness {
        fn new(panes: &[&str]) -> Self {
            Self {
                live_panes: panes.iter().map(|pane| (*pane).to_string()).collect(),
            }
        }
    }

    impl crate::state::owner_gate::PaneLivenessProbe for LeaderPortSeededLiveness {
        fn liveness(&self, pane_id: &str) -> crate::model::enums::PaneLiveness {
            if self.live_panes.contains(pane_id) {
                crate::model::enums::PaneLiveness::Live
            } else {
                crate::model::enums::PaneLiveness::Dead
            }
        }
    }

    // RED — takeover(confirm=true) with no $TMUX_PANE: the Family A positive-source
    // bind gate fires -> refused caller_pane_missing with the bind diagnostic dict.
    // golden probe_claim.py takeover(confirm=True): {ok:false, status:"refused",
    // reason:"caller_pane_missing", caller_pane_id:"", caller_current_command:"",
    // hint:"run team-agent from inside your leader pane (the tmux pane you want to
    // own this team)."}. Current stub returns {ok:true,...} (wrong) -> RED.
    #[test]
    #[ignore = "RED needs $TMUX_PANE ABSENT (Family A bind gate); run `--ignored` in a non-tmux shell. \
                Inside tmux the live-pane resolver would engage. Seam: porter wires leader_port::takeover \
                -> runtime.takeover whose Family A bind refuses caller_pane_missing when $TMUX_PANE missing."]
    fn leader_port_takeover_confirm_without_pane_refuses_caller_pane_missing() {
        if std::env::var_os("TMUX_PANE").is_some() {
            return; // inside tmux the bind gate would pass; this case verifies the missing arm.
        }
        let ws = leader_port_ws("tk_confirm_nopane");
        // seed a resolvable team so the gate reaches the bind step (not team_target_unresolved).
        let st = json!({"session_name": "team-agent-x", "teams": {"team-agent-x": {}}});
        let path = crate::state::persist::runtime_state_path(&ws);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&st).unwrap()).unwrap();
        let v = super::leader_port::takeover(&ws, Some("team-agent-x"), true).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["status"], json!("refused"));
        assert_eq!(v["reason"], json!("caller_pane_missing"));
        assert_eq!(v["caller_pane_id"], json!(""));
        assert_eq!(v["caller_current_command"], json!(""));
        assert_eq!(
            v["hint"],
            json!("run team-agent from inside your leader pane (the tmux pane you want to own this team).")
        );
    }

    // RED — claim_leader(confirm=false) with no $TMUX_PANE: runtime.claim_leader is
    // ALSO Family A (runtime.py:791) -> the bind gate fires FIRST, so the no-pane
    // refusal is caller_pane_missing (NOT the leader-lane "not_in_tmux_pane").
    // golden probe_rtclaim.py. This pins the runtime-vs-leader distinction:
    // the CLI projection must reflect runtime.claim_leader's Family A bind gate.
    // Current stub returns {ok:true, inbox_hint:...} (wrong) -> RED.
    #[test]
    #[ignore = "RED needs $TMUX_PANE ABSENT (Family A bind gate fires first); run `--ignored` in a \
                non-tmux shell. Inside tmux the resolver would engage. Seam: porter wires \
                leader_port::claim_leader -> runtime.claim_leader (Family A) whose bind refuses \
                caller_pane_missing (NOT leader-lane not_in_tmux_pane) when $TMUX_PANE missing."]
    fn leader_port_claim_leader_no_pane_refuses_caller_pane_missing_family_a() {
        if std::env::var_os("TMUX_PANE").is_some() {
            return;
        }
        let ws = leader_port_ws("claim_nopane");
        let v = super::leader_port::claim_leader(&ws, None, false).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["status"], json!("refused"));
        assert_eq!(
            v["reason"],
            json!("caller_pane_missing"),
            "runtime.claim_leader (Family A) bind gate -> caller_pane_missing, NOT not_in_tmux_pane"
        );
        assert_eq!(v["caller_pane_id"], json!(""));
        assert_eq!(
            v["hint"],
            json!("run team-agent from inside your leader pane (the tmux pane you want to own this team).")
        );
    }

    // RED — leader_identity(): CLI directly emits leader.leader_identity's 9-key
    // dict (runtime.leader_identity is imported from leader). golden probe_lid.py
    // keys: ok, uuid_prefix, machine_fingerprint, workspace_abspath, os_user,
    // team_id, current_pane_id, last_seen_at, source. Current stub returns
    // {ok:true, team:...} (wrong shape) -> RED.
    #[test]
    fn leader_port_leader_identity_emits_nine_key_dict() {
        let ws = leader_port_ws("identity");
        std::fs::create_dir_all(crate::model::paths::runtime_dir(&ws)).unwrap();
        let v = super::leader_port::leader_identity(&ws, None).unwrap();
        assert_eq!(v["ok"], json!(true));
        let obj = v.as_object().expect("identity → JSON object");
        for key in [
            "ok", "uuid_prefix", "machine_fingerprint", "workspace_abspath",
            "os_user", "team_id", "current_pane_id", "last_seen_at", "source",
        ] {
            assert!(obj.contains_key(key), "golden identity dict must carry '{key}', got {obj:?}");
        }
        // no override/state uuid → source is the leader-plan "derived" string.
        assert_eq!(v["source"], json!("derived"));
        // uuid_prefix is exactly 12 hex chars (derive[:12]).
        let prefix = v["uuid_prefix"].as_str().expect("uuid_prefix str");
        assert_eq!(prefix.len(), 12, "uuid_prefix == derived[:12]");
        assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
        // no team registered + no TMUX_PANE/receiver → these are JSON null.
        if std::env::var_os("TMUX_PANE").is_none() {
            assert_eq!(v["current_pane_id"], serde_json::Value::Null);
        }
        assert_eq!(v["last_seen_at"], serde_json::Value::Null);
    }

    // ── cmd_watch (cli/adapters.rs:58) [RED] — today a CmdResult::none() no-op ───────────────────────
    // Golden cmd_watch (cli/commands.py:103-109) DELEGATES to run_watch(workspace.resolve(), team) and
    // exits 0. Golden run_watch (watch/__init__.py:25-37) is a `while True` LIVE TAIL that streams
    // render_event_line output (collect_watch_lines) for the watched team. The Rust cmd_watch returns
    // CmdResult::none() — a no-op that never touches the watch subsystem. The watch LINE SHAPE is itself
    // byte-locked by coordinator::render_event_line (coordinator/tests.rs GROUP H); the cli contract here
    // is that cmd_watch must DELEGATE and surface those rendered lines. Seeded a result_received event ->
    // golden render = "result_received: <agent> -> <summary>" (render_event_line: agent_id + summary[:80]).
    //
    // RED confirmed TODAY: cmd_watch=none() returns immediately (no watch line) -> the assertion fails
    // (and does NOT hang). PORTER: wire cmd_watch -> coordinator::run_watch(resolved workspace, team,
    // interval, sink). Since golden's tail is `while True`, the cli port needs a TERMINATING / bounded
    // entry to be both byte-parity AND unit-testable (collect the current watch lines into the CmdResult,
    // or stream to stdout with a bounded test seam) — a blocking unit test is unacceptable.
    #[test]
    fn cmd_watch_delegates_and_surfaces_rendered_watch_lines_not_noop() {
        let ws = tmp_workspace();
        let logs = crate::model::paths::logs_dir(&ws);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("events.jsonl"),
            "{\"event\":\"result_received\",\"agent_id\":\"alpha\",\"summary\":\"did the thing\"}\n",
        )
        .unwrap();
        let r = cmd_watch(&WatchArgs { workspace: ws.clone(), team: None })
            .expect("cmd_watch returns a CmdResult");
        let text = match &r.output {
            CmdOutput::Human(s) => s.clone(),
            CmdOutput::Json(v) => v.to_string(),
            CmdOutput::None => String::new(),
        };
        assert!(
            text.contains("result_received: alpha -> did the thing"),
            "cmd_watch must DELEGATE to the watch subsystem (run_watch -> render_event_line) and surface the \
             rendered watch line; today it is a CmdResult::none() no-op. got output={:?}",
            r.output
        );
    }

    // D7 [WARN] — WAVE-2 Lane B: empty caller binds refuse as caller_pane_missing.
    #[test]
    #[serial_test::serial(env)]
    fn d7_lease_refusal_dict_is_golden_minimal_four_keys() {
        use std::sync::Mutex;
        static D7_ENV: Mutex<()> = Mutex::new(());
        let _g = D7_ENV.lock().unwrap_or_else(|p| p.into_inner());
        let _env = EnvGuard::set(&[
            ("TMUX_PANE", Some("")), // empty caller -> caller_pane_missing via the lease path
            ("TEAM_AGENT_LEADER_PANE_ID", None),
        ]);
        let ws = tmp_workspace();
        crate::state::persist::save_runtime_state(&ws, &json!({})).unwrap(); // claim_leader loads state
        let result = super::leader_port::claim_leader(&ws, None, false);
        let v = result.expect("claim_leader projection");
        let obj = v.as_object().expect("lease dict");
        assert_eq!(
            obj.get("reason").and_then(|r| r.as_str()),
            Some("caller_pane_missing"),
            "precondition: empty caller -> caller_pane_missing; got {v:?}"
        );
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["ok", "status", "reason", "caller_pane_id", "caller_current_command", "hint"]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "golden caller_pane_missing refusal key set"
        );
    }

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(values: &[(&'static str, Option<&'static str>)]) -> Self {
            let previous = values
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for (key, value) in values {
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..).rev() {
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
            }
        }
    }
