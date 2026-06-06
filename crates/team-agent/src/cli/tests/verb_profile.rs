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
    std::fs::write(dir.join("AGENTS.md"), "# Team Agent Profile Secret Boundary\n").unwrap();
    std::fs::write(dir.join("CLAUDE.md"), "# Team Agent Profile Secret Boundary\n").unwrap();
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
//   * init NAME --workspace . --team TEAM --auth-mode choices(sorted AUTH_MODES) --json
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
//   codex_sub.env = "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\n" (mode 0600),
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
    assert_eq!(code, ExitCode::Ok, "`profile init ... --json` must route and exit 0");

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
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\n",
        "subscription profile template body must match golden"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("codex_sub.example.env")).unwrap(),
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\n",
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
    assert_eq!(second, ExitCode::Ok, "profile init is idempotent and still exits 0");
    assert_eq!(
        std::fs::read_to_string(dir.join("codex_sub.env")).unwrap(),
        "AUTH_MODE=subscription\nPROFILE_NAME=codex_sub\n",
        "idempotent init must not rewrite existing profile bytes"
    );
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
    assert_eq!(existing, ExitCode::Ok, "profile doctor existing profile must exit 0");

    let missing = run(
        &profile_argv(&[
            "profile",
            "doctor",
            "missing",
            "--workspace",
            ".",
            "--json",
        ]),
        &ws,
    );
    assert_eq!(missing, ExitCode::Error, "profile doctor missing profile must exit 1");
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
    assert_eq!(code, ExitCode::Ok, "profile show existing profile must exit 0");

    let missing = run(
        &profile_argv(&["profile", "show", "missing", "--workspace", ".", "--json"]),
        &ws,
    );
    assert_eq!(missing, ExitCode::Error, "profile show missing profile must exit 1");

    let code_human = run(
        &profile_argv(&["profile", "show", "api_prof", "--workspace", "."]),
        &ws,
    );
    assert_eq!(code_human, ExitCode::Ok, "profile show human output path must route");
    let _ = std::fs::remove_dir_all(&ws);
}
