//! C1 CommandSpec RED contracts.
//!
//! References:
//! - `.team/artifacts/c1-command-spec-design.md` section 7 RED1-RED5.
//! - Leader supplement: send failure or pending output must guide users to
//!   `inbox` because `inbox` is secondary but operationally important.
//!
//! User story: default help is a small user-facing command surface; surviving
//! compatibility aliases remain exact-invocation compatible, and recovery
//! failures guide users to discoverable repair paths.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use team_agent::state::persist::save_runtime_state;

const DEFAULT_COMMANDS: &[&str] = &[
    "quick-start",
    "send",
    "status",
    "collect",
    "restart",
    "shutdown",
    "add-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "diagnose",
    "claim-leader",
    "takeover",
    "attach-leader",
];

const HIDDEN_FROM_DEFAULT_HELP: &[&str] = &[
    "init",
    "start",
    "stop",
    "restart-agent",
    "fallback-send-leader",
    "fallback-report-result",
    "settle",
    "validate-result",
    "stuck-list",
    "stuck-cancel",
    "acknowledge-idle",
    "repair-state",
    "leaders",
    "doctor",
    "attach-app-server-leader",
    "remove-agent",
    "fork-agent",
    "allow-peer-talk",
    "approvals",
    "profile",
    "install-skill",
    "inbox",
    "identity",
    "watch",
    "sessions",
    "compile",
    "validate",
    "preflight",
    "wait-ready",
    "e2e",
    "peek",
    "coordinator",
];

const COMPAT_HIDDEN_COMMANDS: &[(&str, &[&str])] = &[
    ("stop", &["shutdown"]),
    ("restart-agent", &["reset-agent"]),
    ("start", &["quick-start", "restart"]),
    ("init", &["quick-start"]),
];

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn red1_default_help_contracts_to_core_guided_surface() {
    let case = Case::new("red1-help");
    let output = case.run_ta(&["--help"]);
    let help = stdout(&output);
    assert!(
        output.status.success(),
        "RED1 setup: team-agent --help must run before C1 can judge help contraction; status={} stderr={}",
        output.status,
        stderr(&output)
    );

    let visible = visible_default_commands(&help);
    let expected: BTreeSet<String> = DEFAULT_COMMANDS.iter().map(|s| (*s).to_string()).collect();
    let hidden: BTreeSet<String> = HIDDEN_FROM_DEFAULT_HELP
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let missing: Vec<_> = expected.difference(&visible).cloned().collect();
    let leaked: Vec<_> = hidden.intersection(&visible).cloned().collect();

    assert!(
        visible.len() <= 15 && missing.is_empty() && leaked.is_empty(),
        "RED1: default help must show only the C1 core/guided surface (<=15 names, including the 14 design names) and hide secondary/dev/compat commands.\nvisible_count={}\nvisible={:?}\nmissing_required={:?}\nleaked_hidden={:?}\nhelp=\n{}",
        visible.len(),
        visible,
        missing,
        leaked,
        help
    );
    let lower = help.to_lowercase();
    assert!(
        lower.contains("provider")
            && lower.contains("codex")
            && lower.contains("claude")
            && lower.contains("copilot"),
        "RED1: default help must render provider launchers as a launcher group, not as counted commands; help=\n{help}"
    );
}

#[test]
fn red2_command_registry_covers_current_dispatch_and_replaces_source_scans() {
    let emit = read_repo_file("crates/team-agent/src/cli/emit.rs");
    let spec_path = repo_root().join("crates/team-agent/src/cli/spec.rs");
    let dispatch_names = current_command_surface_from_emit(&emit);
    let mut failures = Vec::new();

    if !spec_path.exists() {
        failures.push(format!(
            "missing `src/cli/spec.rs`; registry must cover {} current dispatch/spec-only/provider names derived from emit.rs: {}",
            dispatch_names.len(),
            dispatch_names.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    } else {
        let spec = std::fs::read_to_string(&spec_path).expect("read cli/spec.rs");
        for required in [
            "CommandSpec",
            "CommandTier",
            "CommandCategory",
            "CommandKind",
            "TokenUsage",
            "COMMAND_SPECS",
        ] {
            if !spec.contains(required) {
                failures.push(format!(
                    "registry is missing required type/table `{required}`"
                ));
            }
        }
        for command in &dispatch_names {
            match spec_block_for(&spec, command) {
                Some(block) => {
                    for field in ["tier", "category", "token_usage", "usage", "summary"] {
                        if !block.contains(field) {
                            failures.push(format!(
                                "`{command}` CommandSpec is missing field `{field}`; block={block:?}"
                            ));
                        }
                    }
                }
                None => failures.push(format!(
                    "current command `{command}` is missing from COMMAND_SPECS"
                )),
            }
        }
    }

    for forbidden in [
        "fn source_dispatch_commands",
        "source_dispatch_commands()",
        "split_once(\"fn dispatch(\")",
        "split_once(\"const DISPATCH_COMMANDS\")",
    ] {
        if emit.contains(forbidden) {
            failures.push(format!(
                "old source-text dispatch scanner remains in cli/emit.rs: `{forbidden}`"
            ));
        }
    }
    if !emit.contains("COMMAND_SPECS") {
        failures.push(
            "cli/emit.rs must use COMMAND_SPECS for help/known-command/suggestion surfaces"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "RED2: C1 requires a registry-complete CommandSpec catalog and no legacy source-text command scanner.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red3_observation_a_commands_have_terminal_tiers_not_placeholders() {
    let spec_path = repo_root().join("crates/team-agent/src/cli/spec.rs");
    assert!(
        spec_path.exists(),
        "RED3: C1 observation-A terminal tier decisions require cli/spec.rs; missing {}",
        spec_path.display()
    );
    let spec = std::fs::read_to_string(&spec_path).expect("read cli/spec.rs");
    let expected = BTreeMap::from([
        ("allow-peer-talk", "secondary"),
        ("approvals", "secondary"),
        ("profile", "secondary"),
        ("install-skill", "secondary"),
        ("init", "compat_hidden"),
    ]);
    let mut failures = Vec::new();
    for (command, tier) in expected {
        let Some(block) = spec_block_for(&spec, command) else {
            failures.push(format!("missing CommandSpec for `{command}`"));
            continue;
        };
        let normalized = normalize(&block);
        let expected_tier = normalize(tier);
        if !(normalized.contains("tier") && normalized.contains(&expected_tier)) {
            failures.push(format!(
                "`{command}` must have terminal tier `{tier}`; block={block:?}"
            ));
        }
        for placeholder in [
            "placeholder",
            "reviewlater",
            "review_later",
            "pendingreview",
            "todo",
        ] {
            if normalized.contains(&normalize(placeholder)) {
                failures.push(format!(
                    "`{command}` carries placeholder/pending governance wording `{placeholder}`; block={block:?}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "RED3: observation-A commands must have exact terminal C1 tiers and no review-later placeholder.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red4_compat_hidden_commands_have_exact_help_with_sunset_and_action() {
    let case = Case::new("red4-compat-help");
    let default_help = stdout(&case.run_ta(&["--help"]));
    let visible = visible_default_commands(&default_help);
    let mut failures = Vec::new();

    for (command, action_terms) in COMPAT_HIDDEN_COMMANDS {
        if visible.contains(*command) {
            failures.push(format!(
                "`{command}` must not appear in default help; visible={visible:?}"
            ));
        }
        let output = case.run_ta(&[command, "--help"]);
        let text = output_text(&output);
        let lower = text.to_lowercase();
        if !output.status.success() {
            failures.push(format!(
                "`team-agent {command} --help` must succeed for exact compat invocation; status={} text={text}",
                output.status
            ));
        }
        if !lower.contains(&format!("usage: team-agent {command}")) {
            failures.push(format!(
                "`{command}` help must include exact usage; text={text}"
            ));
        }
        for required in ["status: hidden compatibility command", "sunset", "action:"] {
            if !lower.contains(required) {
                failures.push(format!(
                    "`{command}` help must include `{required}`; text={text}"
                ));
            }
        }
        if !action_terms.iter().any(|term| lower.contains(term)) {
            failures.push(format!(
                "`{command}` help action must point to one of {:?}; text={text}",
                action_terms
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED4: compat_hidden commands must be absent from default help but keep exact --help with status/sunset/action.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red5_guided_failures_point_to_secondary_discovery_not_hidden_fallbacks() {
    let case = Case::new("red5-guided");
    let default_help = stdout(&case.run_ta(&["--help"]));
    let visible = visible_default_commands(&default_help);
    let mut failures = Vec::new();
    for secondary in ["leaders", "inbox"] {
        if visible.contains(secondary) {
            failures.push(format!(
                "`{secondary}` is a secondary discovery command and must stay out of default help; visible={visible:?}"
            ));
        }
    }

    let send = case.run_ta(&[
        "send",
        "--workspace",
        case.workspace_str(),
        "--to-leader",
        "missing-c1-leader",
        "C1_GUIDED_TOKEN",
        "--json",
    ]);
    let send_text = output_text(&send);
    let send_lower = send_text.to_lowercase();
    for required in ["leaders", "inbox"] {
        if !send_lower.contains(required) {
            failures.push(format!(
                "send --to-leader failure/pending output must guide users to `{required}`; text={send_text}"
            ));
        }
    }

    case.write_rebind_required_state();
    let diagnose = case.run_ta(&["diagnose", "--workspace", case.workspace_str(), "--json"]);
    let diagnose_text = output_text(&diagnose);
    let diagnose_lower = diagnose_text.to_lowercase();
    for required in ["claim-leader", "takeover", "attach-leader"] {
        if !diagnose_lower.contains(required) {
            failures.push(format!(
                "diagnose rebind_required guidance must name `{required}`; text={diagnose_text}"
            ));
        }
    }
    for forbidden in ["fallback-send-leader", "fallback-report-result"] {
        if diagnose_lower.contains(forbidden) {
            failures.push(format!(
                "diagnose must not suggest hidden fallback command `{forbidden}`; text={diagnose_text}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED5: guided recovery must route through secondary discovery and binding commands, with inbox guidance for send failures, never fallback-*.\n{}",
        failures.join("\n")
    );
}

struct Case {
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    workspace_str: String,
}

impl Case {
    fn new(tag: &str) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base).expect("create test tmp base");
        let root = base.join(format!(
            "ta-c1-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create case root");
        let root = std::fs::canonicalize(root).expect("canonical case root");
        let home = root.join("home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(home.join(".team-agent/leaders")).expect("create home registry");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let workspace = std::fs::canonicalize(workspace).expect("canonical workspace");
        let workspace_str = workspace.to_string_lossy().to_string();
        Self {
            root,
            home,
            workspace,
            workspace_str,
        }
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_str
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.workspace);
        for key in [
            "TMUX",
            "TMUX_PANE",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_MACHINE_FINGERPRINT",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn write_rebind_required_state(&self) {
        let state = json!({
            "active_team_key": "c1diag",
            "team_key": "c1diag",
            "teams": {
                "c1diag": {
                    "status": "alive",
                    "team_key": "c1diag",
                    "team_dir": self.workspace.join("TEAM.md"),
                    "session_name": "ta-c1-diagnose-rebind",
                    "agents": {}
                }
            }
        });
        save_runtime_state(&self.workspace, &state).expect("write rebind-required state");
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

fn read_repo_file(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn output_text(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn visible_default_commands(help: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some((_, rest)) = help.split_once("Commands:") {
        let section = rest.split("\n\n").next().unwrap_or(rest);
        for part in section.split(',') {
            let command = part.trim();
            if is_command_name(command) {
                names.insert(command.to_string());
            }
        }
    } else {
        for line in help.lines() {
            let trimmed = line.trim_start();
            if !line.starts_with("  ") || trimmed.starts_with("team-agent ") {
                continue;
            }
            let command = trimmed.split_whitespace().next().unwrap_or_default();
            if is_command_name(command) {
                names.insert(command.to_string());
            }
        }
    }
    for provider in ["codex", "claude", "copilot"] {
        names.remove(provider);
    }
    names
}

fn is_command_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn current_command_surface_from_emit(emit: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for const_name in [
        "DISPATCH_COMMANDS",
        "SPEC_ONLY_HELP_COMMANDS",
        "LEADER_PASSTHROUGH_COMMANDS",
    ] {
        names.extend(parse_const_str_array(emit, const_name));
    }
    names
}

fn parse_const_str_array(source: &str, const_name: &str) -> Vec<String> {
    let marker = format!("const {const_name}:");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing const array {const_name}"));
    let after = &source[start..];
    let array_start = after
        .find("&[")
        .unwrap_or_else(|| panic!("missing array start for {const_name}"));
    let after_array = &after[array_start..];
    let end = after_array
        .find("];")
        .unwrap_or_else(|| panic!("missing array end for {const_name}"));
    let array = &after_array[..end];
    let mut values = Vec::new();
    let mut parts = array.split('"');
    let _ = parts.next();
    while let Some(value) = parts.next() {
        values.push(value.to_string());
        let _ = parts.next();
    }
    values
}

fn spec_block_for(spec: &str, command: &str) -> Option<String> {
    let needle = format!("\"{command}\"");
    let index = spec.find(&needle)?;
    let start = spec[..index]
        .rfind("CommandSpec")
        .unwrap_or(index.saturating_sub(400));
    let end = spec[index + needle.len()..]
        .find("CommandSpec")
        .map(|offset| index + needle.len() + offset)
        .unwrap_or_else(|| spec.len().min(index + 1200));
    Some(spec[start..end].to_string())
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
