//! 0.5.45 naming/addressing RED contracts.
//!
//! User-visible contract: in-team short ids remain the first-class target for
//! positional CLI and worker MCP sends. `--to-name` consumes an explicit
//! `--team` only for a bare name. Typo diagnostics are scope-safe and advisory:
//! they show a copyable candidate but never rewrite or deliver the request.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hermetic_guard::{HermeticTestEnv, CALLER_IDENTITY_ENVS};
use rusqlite::Connection;
use serde_json::{json, Map, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

// RED-1: bare --to-name consumes explicit --team before workspace scanning.

#[test]
#[serial_test::serial(env)]
fn red_1_bare_to_name_explicit_team_scope_matrix() {
    let case = AddressCase::new("red1-scope");

    let naming_token = token("RED1_NAMING");
    let naming = case.cli(&[
        "send",
        "--to-name",
        "beta",
        &naming_token,
        "--workspace",
        path(&case.local),
        "--team",
        "qa-naming",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let naming_json = json_stdout(&naming, "bare --to-name scoped to qa-naming");
    assert_eq!(naming.status.code(), Some(0), "RED-1: {naming_json}");
    assert_eq!(naming_json["team_key"], json!("qa-naming"));
    assert_eq!(naming_json["target"], json!("beta"));
    assert!(naming_json["message_id"].as_str().is_some());
    assert_eq!(case.message_count(&naming_token), 1);
    case.assert_pane_not_contains("local-naming-beta", &naming_token);
    case.assert_pane_not_contains("local-sibling-beta", &naming_token);

    let sibling_token = token("RED1_SIBLING");
    let sibling = case.cli(&[
        "send",
        "--to-name",
        "beta",
        &sibling_token,
        "--workspace",
        path(&case.local),
        "--team",
        "qa-sibling",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let sibling_json = json_stdout(&sibling, "bare --to-name scoped to qa-sibling");
    assert_eq!(sibling.status.code(), Some(0), "RED-1: {sibling_json}");
    assert_eq!(sibling_json["team_key"], json!("qa-sibling"));
    assert_eq!(sibling_json["target"], json!("beta"));
    assert!(sibling_json["message_id"].as_str().is_some());
    assert_eq!(case.message_count(&sibling_token), 1);
    case.assert_pane_not_contains("local-sibling-beta", &sibling_token);
    case.assert_pane_not_contains("local-naming-beta", &sibling_token);

    let ambiguous_token = token("RED1_AMBIGUOUS");
    let ambiguous = case.cli(&[
        "send",
        "--to-name",
        "beta",
        &ambiguous_token,
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let ambiguous_json = json_stdout(&ambiguous, "bare --to-name without team");
    assert_eq!(ambiguous.status.code(), Some(1));
    assert_eq!(ambiguous_json["reason"], json!("name_ambiguous"));
    let names = candidate_names(&ambiguous_json);
    assert!(
        names.contains(&"qa-naming/beta".to_string()),
        "{ambiguous_json}"
    );
    assert!(
        names.contains(&"qa-sibling/beta".to_string()),
        "{ambiguous_json}"
    );
    case.assert_no_pane_contains(&ambiguous_token);

    let missing_token = token("RED1_MISSING");
    let missing = case.cli(&[
        "send",
        "--to-name",
        "beta",
        &missing_token,
        "--workspace",
        path(&case.local),
        "--team",
        "missing",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let missing_json = json_stdout(&missing, "bare --to-name scoped to missing team");
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(missing_json["reason"], json!("name_not_resolvable"));
    assert!(
        missing_json["error"]
            .as_str()
            .is_some_and(|message| message.contains("missing")),
        "RED-1: missing team refusal must name the selected scope: {missing_json}"
    );
    case.assert_no_pane_contains(&missing_token);
}

// RED-2: every typo entry returns suggestions from only its legal scope.

#[test]
#[serial_test::serial(env)]
fn red_2_positional_typo_suggests_only_selected_team_without_db_write() {
    let case = AddressCase::new("red2-positional");
    let content = token("RED2_POSITIONAL");
    let output = case.cli(&[
        "send",
        "btea",
        &content,
        "--workspace",
        path(&case.local),
        "--team",
        "qa-naming",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let body = json_stdout(&output, "positional typo");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["reason"], json!("target_not_in_team"));
    assert_suggestion(&body, "btea", "beta");
    assert_candidates_stay_in_team(&body, "qa-naming");
    assert_eq!(
        case.message_count(&content),
        0,
        "RED-2: typo must not write DB row"
    );
    case.assert_no_pane_contains(&content);
}

#[test]
#[serial_test::serial(env)]
fn red_2_worker_mcp_typo_suggests_only_owner_visible_peers_without_db_write() {
    let case = AddressCase::new("red2-mcp");
    let content = token("RED2_MCP");
    let (is_error, body) = case.mcp_send("alpha", "qa-naming", "btea", &content);

    assert!(is_error, "RED-2: typo remains an MCP refusal: {body}");
    assert_eq!(body["reason"], json!("peer_not_in_scope"));
    assert_suggestion(&body, "btea", "beta");
    assert_candidates_stay_in_team(&body, "qa-naming");
    assert!(
        !body.to_string().contains("qa-sibling"),
        "RED-2: owner-scoped MCP suggestion leaked sibling team: {body}"
    );
    assert_eq!(case.message_count(&content), 0);
    case.assert_no_pane_contains(&content);
}

#[test]
#[serial_test::serial(env)]
fn red_2_named_team_typo_preserves_request_and_suggests_scoped_address() {
    let case = AddressCase::new("red2-named-team");
    let content = token("RED2_NAMED_TEAM");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-namng/beta",
        &content,
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let body = json_stdout(&output, "named team typo");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["reason"], json!("name_not_resolvable"));
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|v| v.contains("qa-namng")),
        "{body}"
    );
    assert_suggestion(&body, "qa-namng/beta", "qa-naming/beta");
    assert_eq!(case.message_count(&content), 0);
    case.assert_no_pane_contains(&content);
}

#[test]
#[serial_test::serial(env)]
fn red_2_named_role_typo_suggests_scoped_address_but_far_name_stays_empty() {
    let case = AddressCase::new("red2-named-role");
    let typo_content = token("RED2_NAMED_ROLE");
    let typo = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/btea",
        &typo_content,
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let typo_json = json_stdout(&typo, "named role typo");
    assert_eq!(typo.status.code(), Some(1));
    assert!(
        typo_json["error"]
            .as_str()
            .is_some_and(|v| v.contains("btea")),
        "{typo_json}"
    );
    assert_suggestion(&typo_json, "qa-naming/btea", "qa-naming/beta");
    assert_eq!(case.message_count(&typo_content), 0);
    case.assert_no_pane_contains(&typo_content);

    let far_content = token("RED2_FAR");
    let far = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/zzzzzz",
        &far_content,
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let far_json = json_stdout(&far, "far named role typo");
    assert_eq!(far.status.code(), Some(1));
    assert!(
        candidate_names(&far_json).is_empty(),
        "RED-2: far typo must not be guessed: {far_json}"
    );
    assert!(far_json.get("suggested_name").is_none(), "{far_json}");
    assert_eq!(case.message_count(&far_content), 0);
    case.assert_no_pane_contains(&far_content);
}

// RED-3: non-JSON output preserves the typo and a copyable suggestion.

#[test]
#[serial_test::serial(env)]
fn red_3_positional_human_refusal_keeps_typo_and_copyable_suggestion() {
    let case = AddressCase::new("red3-positional");
    let output = case.cli(&[
        "send",
        "btea",
        "RED3_POSITIONAL",
        "--workspace",
        path(&case.local),
        "--team",
        "qa-naming",
        "--no-wait",
        "--no-ack",
    ]);
    let human = combined(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        human.contains("btea"),
        "RED-3: original typo missing: {human}"
    );
    assert!(
        human.to_ascii_lowercase().contains("did you mean"),
        "RED-3: {human}"
    );
    assert!(
        human.contains("beta"),
        "RED-3: copyable short id missing: {human}"
    );
}

#[test]
#[serial_test::serial(env)]
fn red_3_named_team_human_n38_keeps_typo_and_copyable_suggestion() {
    let case = AddressCase::new("red3-team");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-namng/beta",
        "RED3_TEAM",
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
    ]);
    assert_named_human_refusal(&output, "qa-namng", "qa-naming/beta");
}

#[test]
#[serial_test::serial(env)]
fn red_3_named_role_human_n38_keeps_typo_and_copyable_suggestion() {
    let case = AddressCase::new("red3-role");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/btea",
        "RED3_ROLE",
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
    ]);
    assert_named_human_refusal(&output, "btea", "qa-naming/beta");
}

// RED-4: the public help and command spec share the canonical persisted-send surface.

#[test]
#[serial_test::serial(env)]
fn red_4_send_help_and_command_spec_share_all_shapes_and_entry_boundaries() {
    let env = HermeticTestEnv::enter("0545-help");
    let output = env.run_cli(env.root(), &["send", "--help"]);
    assert_eq!(output.status.code(), Some(0), "{}", combined(&output));
    let help = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    for required in [
        "logical recipient",
        "returns after the message is persisted",
    ] {
        assert!(
            help.contains(required),
            "RED-4: send help missing `{required}`; help={help}"
        );
    }
    for hidden_alias in ["--to-name", "--to-leader", "--targets", "--pane"] {
        assert!(
            !help.contains(hidden_alias),
            "RED-4: public send help leaked compatibility alias `{hidden_alias}`; help={help}"
        );
    }

    let specs = source("src/cli/spec.rs").to_ascii_lowercase();
    let send_spec = line_containing(&specs, "name: \"send\"");
    for required in ["persist a message", "logical recipient"] {
        assert!(
            send_spec.contains(required),
            "RED-4: COMMAND_SPECS/help drift; missing {required}; spec={send_spec}"
        );
    }
}

#[test]
#[serial_test::serial(env)]
fn red_4_named_error_actions_use_returned_candidates_not_fake_assembled_status_name() {
    let case = AddressCase::new("red4-action");
    let suggested = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/btea",
        "RED4_SUGGESTED",
        "--workspace",
        path(&case.local),
        "--json",
    ]);
    let suggested_json = json_stdout(&suggested, "named action with suggestion");
    let action = suggested_json["action"].as_str().unwrap_or_default();
    assert!(
        action.contains("qa-naming/beta"),
        "RED-4: Action must point to returned candidate: {suggested_json}"
    );
    assert!(
        !action.contains("canonical <workspace>::<team>/<agent> or <team>/<agent> name from team-agent status --json"),
        "RED-4: Action still claims status has an assembled canonical name: {suggested_json}"
    );

    let far = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/zzzzzz",
        "RED4_FAR",
        "--workspace",
        path(&case.local),
        "--json",
    ]);
    let far_json = json_stdout(&far, "named action without suggestion");
    let far_action = far_json["action"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    for required in ["status --json", "team_key", "agent", "--to-name"] {
        assert!(
            far_action.contains(required),
            "RED-4: no-candidate Action missing `{required}`: {far_json}"
        );
    }
}

// RED-5: current fail-closed/address-precedence behavior remains unchanged.

#[test]
#[serial_test::serial(env)]
fn red_5_positional_team_qualified_target_still_refuses() {
    let case = AddressCase::new("red5-positional-long");
    let output = case.cli(&[
        "send",
        "qa-naming/beta",
        "RED5_POSITIONAL_LONG",
        "--workspace",
        path(&case.local),
        "--team",
        "qa-naming",
        "--json",
    ]);
    let body = json_stdout(&output, "positional long guard");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["reason"], json!("target_not_in_team"));
}

#[test]
#[serial_test::serial(env)]
fn red_5_worker_mcp_team_and_workspace_qualified_targets_still_refuse() {
    let case = AddressCase::new("red5-mcp-long");
    for target in [
        "qa-naming/beta".to_string(),
        format!("{}::qa-naming/beta", case.local.display()),
    ] {
        let (is_error, body) = case.mcp_send("alpha", "qa-naming", &target, "RED5_MCP_LONG");
        assert!(
            is_error,
            "RED-5: long MCP target unexpectedly accepted: {body}"
        );
        assert_eq!(body["reason"], json!("peer_not_in_scope"));
    }
}

#[test]
#[serial_test::serial(env)]
fn red_5_cross_workspace_qualified_address_ignores_local_team_flag() {
    let case = AddressCase::new("red5-cross-workspace");
    let address = format!("{}::qa-naming/beta", case.other.display());
    let content = token("RED5_CROSS_WORKSPACE");
    let output = case.cli(&[
        "send",
        "--to-name",
        &address,
        &content,
        "--workspace",
        path(&case.local),
        "--team",
        "qa-sibling",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let body = json_stdout(&output, "cross-workspace address precedence");
    assert_eq!(output.status.code(), Some(0), "RED-5: {body}");
    assert_eq!(body["target_workspace"], json!(path(&case.other)));
    assert_eq!(body["team_key"], json!("qa-naming"));
    assert_eq!(body["target"], json!("beta"));
    assert!(body["message_id"].as_str().is_some());
    assert_eq!(case.message_count_in(&case.other, &content), 1);
    case.assert_pane_not_contains("other-naming-beta", &content);
    case.assert_pane_not_contains("local-sibling-beta", &content);
}

#[test]
#[serial_test::serial(env)]
fn red_5_team_qualified_address_ignores_conflicting_team_flag() {
    let case = AddressCase::new("red5-team-qualified");
    let content = token("RED5_TEAM_QUALIFIED");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/beta",
        &content,
        "--workspace",
        path(&case.local),
        "--team",
        "qa-sibling",
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let body = json_stdout(&output, "team-qualified address precedence");
    assert_eq!(output.status.code(), Some(0), "RED-5: {body}");
    assert_eq!(body["team_key"], json!("qa-naming"));
    assert_eq!(body["target"], json!("beta"));
    assert!(body["message_id"].as_str().is_some());
    assert_eq!(case.message_count(&content), 1);
    case.assert_pane_not_contains("local-naming-beta", &content);
    case.assert_pane_not_contains("local-sibling-beta", &content);
}

#[test]
#[serial_test::serial(env)]
fn red_5_structurally_invalid_named_address_never_fuzzy_sends() {
    let case = AddressCase::new("red5-invalid");
    let content = token("RED5_INVALID");
    let output = case.cli(&[
        "send",
        "--to-name",
        "team//beta",
        &content,
        "--workspace",
        path(&case.local),
        "--json",
    ]);
    let body = json_stdout(&output, "invalid named address");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["reason"], json!("name_invalid"));
    assert!(
        candidate_names(&body).is_empty(),
        "RED-5: grammar error must not fuzzy rank: {body}"
    );
    assert!(body.get("suggested_name").is_none(), "{body}");
    case.assert_no_pane_contains(&content);
}

#[test]
#[serial_test::serial(env)]
fn red_5_exact_valid_named_short_has_no_suggestion_fields() {
    let case = AddressCase::new("red5-exact");
    let content = token("RED5_EXACT");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/beta",
        &content,
        "--workspace",
        path(&case.local),
        "--no-wait",
        "--no-ack",
        "--json",
    ]);
    let body = json_stdout(&output, "exact named address");
    assert_eq!(output.status.code(), Some(0), "RED-5: {body}");
    for field in ["requested_name", "suggested_name", "candidates"] {
        assert!(
            body.get(field).is_none(),
            "RED-5: exact success leaked {field}: {body}"
        );
    }
    assert_eq!(body["target"], json!("beta"));
    assert!(body["message_id"].as_str().is_some());
    assert_eq!(case.message_count(&content), 1);
    case.assert_pane_not_contains("local-naming-beta", &content);
}

#[test]
#[serial_test::serial(env)]
fn red_5_fuzzy_refusal_writes_no_message_row_and_injects_no_pane() {
    let case = AddressCase::new("red5-fuzzy-no-send");
    let content = token("RED5_FUZZY_NO_SEND");
    let output = case.cli(&[
        "send",
        "--to-name",
        "qa-naming/btea",
        &content,
        "--workspace",
        path(&case.local),
        "--json",
    ]);
    let body = json_stdout(&output, "fuzzy refusal no send");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["reason"], json!("name_not_resolvable"));
    assert_eq!(
        case.message_count(&content),
        0,
        "RED-5: advisory suggestion wrote DB row"
    );
    case.assert_no_pane_contains(&content);
}

// RED-6: one deterministic, thresholded ranking contract is shared with commands.

#[test]
#[serial_test::serial(env)]
fn red_6_similarity_prefix_distance_tie_limit_threshold_and_case_matrix() {
    let case = AddressCase::new("red6-ranking");

    case.set_qa_naming_agents(&["status", "slat"]);
    let prefix = case.positional_typo("stat", "RED6_PREFIX");
    assert_eq!(
        candidate_names(&prefix),
        vec!["status", "slat"],
        "RED-6: prefix match must outrank lower-distance non-prefix: {prefix}"
    );

    case.set_qa_naming_agents(&["beta", "bota"]);
    let tie = case.positional_typo("bata", "RED6_TIE");
    assert_eq!(
        candidate_names(&tie),
        vec!["beta", "bota"],
        "RED-6: stable key must decide equal distance: {tie}"
    );

    case.set_qa_naming_agents(&["aaab", "aaac", "aaad", "aaae"]);
    let limited = case.positional_typo("aaaa", "RED6_LIMIT");
    assert_eq!(
        candidate_names(&limited),
        vec!["aaab", "aaac", "aaad"],
        "RED-6: candidates must be stable and capped at three: {limited}"
    );

    case.set_qa_naming_agents(&["Beta"]);
    let cased = case.positional_typo("BTEA", "RED6_CASE");
    assert_eq!(
        cased["suggested_name"],
        json!("Beta"),
        "RED-6: canonical case must be preserved: {cased}"
    );

    case.set_qa_naming_agents(&["beta"]);
    let far = case.positional_typo("zzzzzz", "RED6_THRESHOLD");
    assert!(
        candidate_names(&far).is_empty(),
        "RED-6: over-threshold match must be empty: {far}"
    );
    assert!(far.get("suggested_name").is_none(), "{far}");
}

#[test]
#[serial_test::serial(env)]
fn red_6_similarity_is_one_pure_source_and_statu_still_suggests_status() {
    let similarity = source("src/model/name_similarity.rs");
    for forbidden in [
        "std::fs",
        "load_runtime_state",
        "MessageStore",
        "tmux",
        "send_message",
    ] {
        assert!(
            !similarity.contains(forbidden),
            "RED-6: pure similarity helper contains {forbidden}"
        );
    }
    assert!(
        similarity.contains("levenshtein"),
        "RED-6: shared helper must own distance calculation"
    );

    let emit = source("src/cli/emit.rs");
    assert!(
        !emit.contains("fn levenshtein"),
        "RED-6: emit.rs kept a second distance implementation"
    );
    assert!(
        emit.contains("name_similarity"),
        "RED-6: subcommand suggestions must reuse shared helper"
    );

    let env = HermeticTestEnv::enter("0545-statu");
    let output = env.run_cli(env.root(), &["statu"]);
    assert_eq!(output.status.code(), Some(1));
    let human = combined(&output).to_ascii_lowercase();
    assert!(
        human.contains("status"),
        "RED-6: existing `statu -> status` suggestion drifted: {human}"
    );
}

struct AddressCase {
    env: HermeticTestEnv,
    local: PathBuf,
    other: PathBuf,
    local_socket: PathBuf,
    other_socket: PathBuf,
    panes: BTreeMap<String, String>,
}

impl AddressCase {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(&format!("0545-{tag}"));
        let local = env.workspace("local");
        let other = env.workspace("other");
        let local_socket = short_socket("local");
        let other_socket = short_socket("other");

        start_tmux_server(
            &local_socket,
            &[
                ("team-qa-naming", "alpha"),
                ("team-qa-naming", "beta"),
                ("team-qa-sibling", "beta"),
            ],
        );
        start_tmux_server(&other_socket, &[("team-qa-naming", "beta")]);

        let mut panes = BTreeMap::new();
        panes.insert(
            "local-naming-alpha".to_string(),
            pane_id(&local_socket, "team-qa-naming", "alpha"),
        );
        panes.insert(
            "local-naming-beta".to_string(),
            pane_id(&local_socket, "team-qa-naming", "beta"),
        );
        panes.insert(
            "local-sibling-beta".to_string(),
            pane_id(&local_socket, "team-qa-sibling", "beta"),
        );
        panes.insert(
            "other-naming-beta".to_string(),
            pane_id(&other_socket, "team-qa-naming", "beta"),
        );

        let case = Self {
            env,
            local,
            other,
            local_socket,
            other_socket,
            panes,
        };
        case.write_local_state(&["alpha", "beta"]);
        case.write_other_state();
        case
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.local, args)
    }

    fn pane(&self, name: &str) -> &str {
        self.panes
            .get(name)
            .unwrap_or_else(|| panic!("missing pane {name}"))
    }

    fn mcp_send(
        &self,
        sender: &str,
        owner_team: &str,
        target: &str,
        content: &str,
    ) -> (bool, Value) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "send_message",
                "arguments": {
                    "to": target,
                    "content": content,
                    "requires_ack": false
                }
            }
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(["mcp-server", "--workspace", path(&self.local)])
            .current_dir(&self.local)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in CALLER_IDENTITY_ENVS
            .iter()
            .copied()
            .chain(["TEAM_AGENT_AGENT_ID", "TEAM_AGENT_AUTH_MODE"])
        {
            command.env_remove(key);
        }
        command
            .env("HOME", self.env.home())
            .env("TEAM_AGENT_ID", sender)
            .env("TEAM_AGENT_AGENT_ID", sender)
            .env("TEAM_AGENT_TEAM_ID", owner_team)
            .env("TEAM_AGENT_OWNER_TEAM_ID", owner_team)
            .env("TEAM_AGENT_WORKSPACE", &self.local);
        let mut child = command.spawn().expect("spawn MCP server");
        {
            let stdin = child.stdin.as_mut().expect("MCP stdin");
            writeln!(
                stdin,
                "{}",
                json!({"jsonrpc":"2.0","id":1,"method":"initialize"})
            )
            .expect("write initialize");
            writeln!(stdin, "{request}").expect("write tools/call");
        }
        let output = child.wait_with_output().expect("wait MCP server");
        assert!(
            output.status.success(),
            "MCP process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["id"] == json!(2))
            .unwrap_or_else(|| {
                panic!(
                    "missing tools/call response: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            });
        let is_error = response["result"]["isError"].as_bool().unwrap_or(false);
        let envelope_text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP envelope text");
        let envelope = serde_json::from_str(envelope_text).expect("MCP JSON envelope");
        (is_error, envelope)
    }

    fn set_qa_naming_agents(&self, ids: &[&str]) {
        self.write_local_state(ids);
    }

    fn positional_typo(&self, requested: &str, label: &str) -> Value {
        let output = self.cli(&[
            "send",
            requested,
            label,
            "--workspace",
            path(&self.local),
            "--team",
            "qa-naming",
            "--no-wait",
            "--no-ack",
            "--json",
        ]);
        let body = json_stdout(&output, "ranking positional typo");
        assert_eq!(
            output.status.code(),
            Some(1),
            "ranking request must stay refused: {body}"
        );
        assert_eq!(body["reason"], json!("target_not_in_team"));
        body
    }

    fn message_count(&self, content: &str) -> i64 {
        self.message_count_in(&self.local, content)
    }

    fn message_count_in(&self, workspace: &Path, content: &str) -> i64 {
        let store = team_agent::message_store::MessageStore::open(workspace)
            .expect("open hermetic message store");
        self.env.assert_store_under_root(&store);
        let conn = Connection::open(store.db_path()).expect("open team.db");
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE content = ?1",
            [content],
            |row| row.get(0),
        )
        .expect("count matching message rows")
    }

    fn assert_pane_not_contains(&self, pane: &str, token: &str) {
        let got = self.capture(pane);
        assert!(
            !got.contains(token),
            "pane {pane} unexpectedly contains {token}: {got}"
        );
    }

    fn assert_no_pane_contains(&self, token: &str) {
        for pane in self.panes.keys() {
            self.assert_pane_not_contains(pane, token);
        }
    }

    fn capture(&self, pane: &str) -> String {
        let socket = if pane.starts_with("other-") {
            &self.other_socket
        } else {
            &self.local_socket
        };
        let output = Command::new("tmux")
            .args([
                "-S",
                path(socket),
                "capture-pane",
                "-p",
                "-S",
                "-200",
                "-t",
                self.pane(pane),
            ])
            .output()
            .expect("capture owned pane");
        assert!(
            output.status.success(),
            "capture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn write_local_state(&self, qa_naming_ids: &[&str]) {
        let naming_agents = agent_map(
            qa_naming_ids,
            &[
                ("alpha", self.pane("local-naming-alpha")),
                ("beta", self.pane("local-naming-beta")),
            ],
        );
        let sibling_agents = agent_map(&["beta"], &[("beta", self.pane("local-sibling-beta"))]);
        write_state(
            &self.local,
            json!({
                "active_team_key": "qa-naming",
                "team_key": "qa-naming",
                "session_name": "team-qa-naming",
                "tmux_endpoint": path(&self.local_socket),
                "tmux_socket": path(&self.local_socket),
                "agents": naming_agents.clone(),
                "tasks": [],
                "teams": {
                    "qa-naming": team_state(
                        "qa-naming",
                        "team-qa-naming",
                        &self.local_socket,
                        naming_agents
                    ),
                    "qa-sibling": team_state(
                        "qa-sibling",
                        "team-qa-sibling",
                        &self.local_socket,
                        sibling_agents
                    )
                }
            }),
        );
    }

    fn write_other_state(&self) {
        let agents = agent_map(&["beta"], &[("beta", self.pane("other-naming-beta"))]);
        write_state(
            &self.other,
            json!({
                "active_team_key": "qa-naming",
                "team_key": "qa-naming",
                "session_name": "team-qa-naming",
                "tmux_endpoint": path(&self.other_socket),
                "tmux_socket": path(&self.other_socket),
                "agents": agents.clone(),
                "tasks": [],
                "teams": {
                    "qa-naming": team_state(
                        "qa-naming",
                        "team-qa-naming",
                        &self.other_socket,
                        agents
                    )
                }
            }),
        );
    }
}

impl Drop for AddressCase {
    fn drop(&mut self) {
        kill_tmux_server(&self.local_socket);
        kill_tmux_server(&self.other_socket);
    }
}

fn agent_map(ids: &[&str], live: &[(&str, &str)]) -> Value {
    let mut agents = Map::new();
    for id in ids {
        let pane_id = live
            .iter()
            .find_map(|(name, pane)| (*name == *id).then_some(*pane))
            .map(str::to_string)
            .unwrap_or_else(|| format!("%state-{id}"));
        agents.insert(
            (*id).to_string(),
            json!({
                "agent_id": id,
                "status": "running",
                "pane_id": pane_id,
                "window": id,
                "window_name": id
            }),
        );
    }
    Value::Object(agents)
}

fn team_state(team_key: &str, session_name: &str, socket: &Path, agents: Value) -> Value {
    json!({
        "team_key": team_key,
        "status": "alive",
        "session_name": session_name,
        "tmux_endpoint": path(socket),
        "tmux_socket": path(socket),
        "agents": agents,
        "tasks": []
    })
}

fn write_state(workspace: &Path, value: Value) {
    let runtime = workspace.join(".team/runtime");
    fs::create_dir_all(&runtime).expect("create runtime dir");
    fs::write(
        runtime.join("state.json"),
        serde_json::to_vec_pretty(&value).expect("serialize state"),
    )
    .expect("write state");
}

fn start_tmux_server(socket: &Path, panes: &[(&str, &str)]) {
    let _ = fs::remove_file(socket);
    for (index, (session, window)) in panes.iter().enumerate() {
        let mut command = Command::new("tmux");
        command.args(["-S", path(socket)]);
        if index == 0 || panes[..index].iter().all(|(seen, _)| seen != session) {
            command.args(["new-session", "-d", "-s", session, "-n", window, "/bin/cat"]);
        } else {
            command.args(["new-window", "-d", "-t", session, "-n", window, "/bin/cat"]);
        }
        let output = command.output().expect("start owned tmux pane");
        assert!(
            output.status.success(),
            "tmux start failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn kill_tmux_server(socket: &Path) {
    let _ = Command::new("tmux")
        .args(["-S", path(socket), "kill-server"])
        .output();
    for _ in 0..20 {
        if !socket.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(socket);
}

fn pane_id(socket: &Path, session: &str, window: &str) -> String {
    let target = format!("{session}:{window}");
    let output = Command::new("tmux")
        .args([
            "-S",
            path(socket),
            "list-panes",
            "-t",
            &target,
            "-F",
            "#{pane_id}",
        ])
        .output()
        .expect("list owned pane");
    assert!(
        output.status.success(),
        "pane lookup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn short_socket(tag: &str) -> PathBuf {
    if let Some(root) = std::env::var_os("TEAM_AGENT_TEST_TMP").map(PathBuf::from) {
        fs::create_dir_all(&root).expect("create TEAM_AGENT_TEST_TMP socket root");
        return root.join(format!(
            "ta45-{tag}-{}-{}.sock",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
    }
    hermetic_guard::short_tmux_socket(&format!("0545-{tag}"))
}

fn token(prefix: &str) -> String {
    format!(
        "{prefix}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn json_stdout(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!(
            "{label}: stdout was not JSON: {error}; stdout={stdout:?}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn combined(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_suggestion(body: &Value, requested: &str, suggested: &str) {
    assert_eq!(
        body["requested_name"],
        json!(requested),
        "requested typo missing: {body}"
    );
    assert_eq!(
        body["suggested_name"],
        json!(suggested),
        "scope-safe suggestion missing: {body}"
    );
    assert!(
        candidate_names(body).contains(&suggested.to_string()),
        "best suggestion must also be in candidates: {body}"
    );
}

fn assert_candidates_stay_in_team(body: &Value, team: &str) {
    let candidates = body["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("missing candidates: {body}"));
    assert!(!candidates.is_empty(), "missing candidates: {body}");
    for candidate in candidates {
        assert_eq!(
            candidate["team_key"],
            json!(team),
            "candidate leaked outside {team}: {body}"
        );
        assert_eq!(
            candidate["advisory"],
            json!(true),
            "candidate must remain advisory: {body}"
        );
    }
}

fn candidate_names(body: &Value) -> Vec<String> {
    body.get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|candidate| candidate.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn assert_named_human_refusal(output: &Output, typo: &str, suggestion: &str) {
    let human = combined(output);
    assert_eq!(output.status.code(), Some(1));
    for marker in ["Error:", "Action:", "Log:"] {
        assert!(
            human.contains(marker),
            "RED-3: N38 marker {marker} missing: {human}"
        );
    }
    assert!(
        human.contains(typo),
        "RED-3: original typo missing: {human}"
    );
    assert!(
        human.to_ascii_lowercase().contains("did you mean"),
        "RED-3: {human}"
    );
    assert!(
        human.contains(suggestion),
        "RED-3: copyable token missing: {human}"
    );
}

fn source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn line_containing<'a>(source: &'a str, needle: &str) -> &'a str {
    source
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing line containing {needle}"))
}

fn path(value: &Path) -> &str {
    value.to_str().expect("UTF-8 fixture path")
}
