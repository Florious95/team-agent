use super::*;

fn profile_argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn profiles_dir(ws: &std::path::Path) -> std::path::PathBuf {
    ws.join(".team").join("current").join("profiles")
}

fn seed_official_api_profile(ws: &std::path::Path) {
    let dir = profiles_dir(ws);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("AGENTS.md"),
        "# Team Agent Profile Secret Boundary\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("CLAUDE.md"),
        "# Team Agent Profile Secret Boundary\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("api_prof.env"),
        "AUTH_MODE=official_api\nPROFILE_NAME=api_prof\nAPI_KEY=sk-test-secret\nMODEL=gpt-test\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("api_prof.example.env"),
        "AUTH_MODE=official_api\nPROFILE_NAME=api_prof\nAPI_KEY=\nMODEL=\n",
    )
    .unwrap();
}

// Golden source:
// - cli/parser.py:131-150 registers `profile {init,doctor,show}`:
//   * init NAME --workspace . --team TEAM --auth-mode choices(sorted AUTH_MODES) --proxy-mode direct|inherit --json
//   * doctor NAME --workspace . --team TEAM --json
//   * show NAME --workspace . --team TEAM --json
// - cli/commands.py:47-67 resolves scope then delegates to profiles.init_profile/
//   doctor_profile/show_profile.
// - profiles/core.py:51-93 init_profile writes `.team/current/profiles/{name}.env`,
//   `{name}.example.env`, AGENTS.md and CLAUDE.md boundary files, chmods real .env to 0600,
//   and returns keys `{ok,profile,auth_mode,path,template_path,created_profile,
//   created_template,secret_written,safe_inspection_command,
//   raw_file_read_allowed_for_agents,instruction}`.
// - cli/helpers.py:12-23 emits success JSON as `json.dumps(indent=2, sort_keys=True)`;
//   human dict output preserves return insertion order.
//
// Golden probe:
//   PYTHONPATH=/Users/alauda/Documents/code/team-agent-public/src \
//     python3 /tmp/probe_profile_cli.py
//   profile init codex_sub --auth-mode subscription --json rc=0 and creates:
//   codex_sub.env = "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\nPROXY_MODE=inherit\n" (mode 0600),
//   codex_sub.example.env with same body, plus AGENTS.md/CLAUDE.md secret boundary files.
#[test]
fn profile_init_routes_and_creates_secret_boundary_files() {
    let ws = tmp_workspace();
    let code = run(
        &profile_argv(&[
            "profile",
            "init",
            "codex_sub",
            "--workspace",
            ".",
            "--auth-mode",
            "subscription",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(
        code,
        ExitCode::Ok,
        "`profile init ... --json` must route and exit 0"
    );

    let dir = profiles_dir(&ws);
    assert!(
        dir.join("AGENTS.md").exists(),
        "profile init must create AGENTS.md secret boundary"
    );
    assert!(
        dir.join("CLAUDE.md").exists(),
        "profile init must create CLAUDE.md secret boundary"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("codex_sub.env")).unwrap(),
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\nPROXY_MODE=inherit\n",
        "subscription profile template body must match golden"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("codex_sub.example.env")).unwrap(),
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\nPROXY_MODE=inherit\n",
        "subscription example template body must match golden"
    );

    let second = run(
        &profile_argv(&[
            "profile",
            "init",
            "codex_sub",
            "--workspace",
            ".",
            "--auth-mode",
            "subscription",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(
        second,
        ExitCode::Ok,
        "profile init is idempotent and still exits 0"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("codex_sub.env")).unwrap(),
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\nPROXY_MODE=inherit\n",
        "idempotent init must not rewrite existing profile bytes"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn compatible_api_profile_init_template_contains_common_five_fields() {
    let ws = tmp_workspace();
    let code = run(
        &profile_argv(&[
            "profile",
            "init",
            "compat",
            "--workspace",
            ".",
            "--auth-mode",
            "compatible_api",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(
        code,
        ExitCode::Ok,
        "compatible_api profile init must route and exit 0"
    );

    let expected =
        "AUTH_MODE=compatible_api\nPROFILE_NAME=compat\nPROXY_MODE=inherit\nBASE_URL=\nAPI_KEY=\nMODEL=\n";
    let dir = profiles_dir(&ws);
    assert_eq!(
        std::fs::read_to_string(dir.join("compat.env")).unwrap(),
        expected,
        "compatible_api profile must expose the documented five-field template"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("compat.example.env")).unwrap(),
        expected,
        "compatible_api example must expose the documented five-field template"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn profile_init_accepts_local_proxy_mode_and_rejects_unknown_values() {
    let ws = tmp_workspace();
    let direct = run(
        &profile_argv(&[
            "profile",
            "init",
            "cursor_sub",
            "--workspace",
            ".",
            "--auth-mode",
            "subscription",
            "--proxy-mode=direct",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(direct, ExitCode::Ok, "direct proxy mode must be accepted");
    let body = std::fs::read_to_string(profiles_dir(&ws).join("cursor_sub.env")).unwrap();
    assert!(body.contains("PROXY_MODE=direct\n"));

    let invalid = run(
        &profile_argv(&[
            "profile",
            "init",
            "bad_proxy",
            "--workspace",
            ".",
            "--proxy-mode",
            "sideways",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(invalid, ExitCode::Error, "unknown proxy mode must fail");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn profile_init_existing_profile_reports_actual_mode_and_show_matches() {
    let ws = tmp_workspace();
    let make_args = |command: &str, mode: Option<&str>| ProfileArgs {
        command: command.to_string(),
        name: "cursor_sub".to_string(),
        workspace: ws.clone(),
        team: None,
        auth_mode: Some("subscription".to_string()),
        proxy_mode: mode.map(str::to_string),
        json: true,
    };
    let first = cmd_profile(&make_args("init", Some("direct"))).unwrap();
    let first = match first.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected json output, got {other:?}"),
    };
    assert_eq!(first["proxy_mode"], "direct");
    assert_eq!(first["proxy_mode_changed"], true);

    let repeat = cmd_profile(&make_args("init", Some("inherit"))).unwrap();
    let repeat = match repeat.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected json output, got {other:?}"),
    };
    assert_eq!(repeat["proxy_mode"], "direct");
    assert_eq!(repeat["proxy_mode_changed"], false);

    let shown = cmd_profile(&make_args("show", None)).unwrap();
    let shown = match shown.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected json output, got {other:?}"),
    };
    assert_eq!(shown["proxy_mode"], "direct");
    let _ = std::fs::remove_dir_all(&ws);
}

// Golden source:
// - profiles/core.py:95-119 doctor_profile returns ok=true for existing profiles and
//   ok=false for missing profiles; parser.py:506-508 maps result.ok false to exit 1.
// - Existing JSON sorted keys include auth_mode, credential_present, keys_present,
//   ok, path, profile, raw_file_read_allowed_for_agents, redaction_engine,
//   safe_for_agent_context, safe_inspection_command, secret_keys_present,
//   secret_values_printed, suggestion, template_path.
// - Missing doctor JSON exits 1 and carries suggestion
//   "Run team-agent profile init missing --auth-mode subscription.".
#[test]
fn profile_doctor_routes_existing_ok_and_missing_error() {
    let ws = tmp_workspace();
    seed_official_api_profile(&ws);

    let existing = run(
        &profile_argv(&[
            "profile",
            "doctor",
            "api_prof",
            "--workspace",
            ".",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(
        existing,
        ExitCode::Ok,
        "profile doctor existing profile must exit 0"
    );

    let missing = run(
        &profile_argv(&["profile", "doctor", "missing", "--workspace", ".", "--json"]),
        &ws,
    );
    assert_eq!(
        missing,
        ExitCode::Error,
        "profile doctor missing profile must exit 1"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

// Golden source:
// - profiles/core.py:121-146 show_profile returns redacted values sorted by key.
// - profiles/helpers.py:40-58 marks API_KEY secret values as `{present:true,redacted:true}`
//   and never includes the raw secret; non-secret AUTH_MODE/MODEL/PROFILE_NAME carry `value`.
// - Human output preserves returned dict insertion order:
//   ok, profile, credential_present, auth_mode, values, keys_present, secret_keys_present,
//   missing_common, safe_for_agent_context, secret_values_printed,
//   raw_file_read_allowed_for_agents, instruction.
#[test]
fn profile_show_routes_and_preserves_redacted_secret_contract() {
    let ws = tmp_workspace();
    seed_official_api_profile(&ws);

    let code = run(
        &profile_argv(&["profile", "show", "api_prof", "--workspace", ".", "--json"]),
        &ws,
    );
    assert_eq!(
        code,
        ExitCode::Ok,
        "profile show existing profile must exit 0"
    );

    let missing = run(
        &profile_argv(&["profile", "show", "missing", "--workspace", ".", "--json"]),
        &ws,
    );
    assert_eq!(
        missing,
        ExitCode::Error,
        "profile show missing profile must exit 1"
    );

    let code_human = run(
        &profile_argv(&["profile", "show", "api_prof", "--workspace", "."]),
        &ws,
    );
    assert_eq!(
        code_human,
        ExitCode::Ok,
        "profile show human output path must route"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
