#![allow(clippy::unwrap_used)]
use super::*;
use crate::model::spec;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_base() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ta_rs_compile_{}_{}", std::process::id(), n))
}

/// Build `<tmp>/.team/current/{TEAM.md, agents/*, profiles/*}` and return the
/// team dir. `team_workspace(team_dir)` → `<tmp>` (parent is `.team`).
fn build_team(team_md: &str, roles: &[(&str, &str)], profiles: &[(&str, &str)]) -> PathBuf {
    let team = unique_base().join(".team").join("current");
    fs::create_dir_all(team.join("agents")).unwrap();
    fs::create_dir_all(team.join("profiles")).unwrap();
    fs::write(team.join("TEAM.md"), team_md).unwrap();
    for (name, text) in roles {
        fs::write(team.join("agents").join(name), text).unwrap();
    }
    for (name, text) in profiles {
        fs::write(team.join("profiles").join(name), text).unwrap();
    }
    team
}

fn write_tmp(name: &str, text: &str) -> PathBuf {
    let base = unique_base();
    fs::create_dir_all(&base).unwrap();
    let p = base.join(name);
    fs::write(&p, text).unwrap();
    p
}

/// `json.dumps(v, sort_keys=False, separators=(",",":"))` for a `yaml::Value`:
/// preserves Map insertion order; string escaping matches Python json (ASCII
/// content here). Locks both VALUE and KEY INSERTION ORDER of the spec dict.
fn compact_json(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Str(s) => serde_json::to_string(s).unwrap(),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(compact_json).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Map(pairs) => {
            let inner: Vec<String> = pairs
                .iter()
                .map(|(k, val)| format!("{}:{}", serde_json::to_string(k).unwrap(), compact_json(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Render spec to compact JSON with the env-dependent workspace path templated
/// to `__WS__` (appears as both `team.workspace` and each `working_directory`).
fn templated_compact_json(spec: &Value) -> String {
    let ws = spec
        .get("team")
        .and_then(|t| t.get("workspace"))
        .and_then(Value::as_str)
        .expect("spec.team.workspace must be a string");
    compact_json(spec).replace(ws, "__WS__")
}

fn workspace_of(spec: &Value) -> String {
    spec.get("team")
        .and_then(|t| t.get("workspace"))
        .and_then(Value::as_str)
        .unwrap()
        .to_string()
}

// ───────────────────── fixtures (byte-identical to the Python probe) ─────────────────────

const TEAM_BASE: &str = "\
---
name: doc-team
objective: Compile role docs.
provider: codex
model: gpt-5.5
---

Document-driven team.
";

const ROLE_NOPROFILE: &str = "\
---
name: implementer
role: Implementation Engineer
provider: codex
model: gpt-5.5
auth_mode: subscription
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Implement bounded tasks and report result_envelope_v1.
";

// ───────────────────────────── read_front_matter ─────────────────────────────

#[test]
fn front_matter_no_marker_returns_empty_meta_and_full_text() {
    // No leading "---\n" → ({}, text) verbatim (compiler.py:175-176).
    let p = write_tmp("no_marker.md", "hello\nworld\n");
    let (meta, body) = read_front_matter(&p).unwrap();
    assert_eq!(meta, Value::Map(vec![]));
    assert_eq!(body, "hello\nworld\n");
}

#[test]
fn front_matter_basic_splits_meta_and_lstrips_body() {
    let p = write_tmp("basic.md", "---\nname: x\nrole: R\n---\n\nbody line\n");
    let (meta, body) = read_front_matter(&p).unwrap();
    assert_eq!(
        meta,
        Value::Map(vec![
            ("name".to_string(), Value::Str("x".to_string())),
            ("role".to_string(), Value::Str("R".to_string())),
        ])
    );
    assert_eq!(body, "body line\n");
}

#[test]
fn front_matter_empty_block_is_empty_map() {
    // "---\n\n---\nbody\n": closing marker at the blank line → raw "" → {} ; body "body\n".
    let p = write_tmp("empty.md", "---\n\n---\nbody\n");
    let (meta, body) = read_front_matter(&p).unwrap();
    assert_eq!(meta, Value::Map(vec![]));
    assert_eq!(body, "body\n");
}

#[test]
fn front_matter_body_lstrip_strips_only_newlines() {
    // body.lstrip("\n") removes leading NEWLINES but keeps the 2-space indent.
    let p = write_tmp("lstrip.md", "---\nname: x\n---\n\n\n  indented body\n");
    let (meta, body) = read_front_matter(&p).unwrap();
    assert_eq!(meta, Value::Map(vec![("name".to_string(), Value::Str("x".to_string()))]));
    assert_eq!(body, "  indented body\n");
}

#[test]
fn front_matter_unterminated_errors() {
    let p = write_tmp("unterminated.md", "---\nname: x\n");
    let err = read_front_matter(&p).unwrap_err();
    assert!(
        err.to_string().contains("unterminated front matter"),
        "got: {err}"
    );
}

#[test]
fn front_matter_non_object_errors() {
    // A YAML list in the block → "front matter must be a YAML object" (compiler.py:183-184).
    let p = write_tmp("list.md", "---\n- a\n- b\n---\nbody\n");
    let err = read_front_matter(&p).unwrap_err();
    assert!(
        err.to_string().contains("front matter must be a YAML object"),
        "got: {err}"
    );
}

// ───────────────────────────── compile_team: full dict parity ─────────────────────────────

// Golden = Python `json.dumps(compile_team(team)["spec"], sort_keys=False,
// separators=(",",":"))` with the workspace path templated to __WS__.
// (team-agent-public v0.2.11, /tmp/probe_compiler.py.)

const BASE_NOPROFILE_JSON: &str = r#"{"version":1,"team":{"name":"doc-team","mode":"supervisor_worker","objective":"Compile role docs.","workspace":"__WS__"},"leader":{"id":"leader","role":"leader","provider":"codex","model":"gpt-5.5","tools":["fs_read","fs_list","mcp_team"],"context_policy":{"keep_user_thread":true,"receive_worker_outputs":"business_messages_and_short_summaries","max_worker_result_tokens":2000}},"agents":[{"id":"implementer","role":"Implementation Engineer","provider":"codex","model":"gpt-5.5","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Implement bounded tasks and report result_envelope_v1.","file":null},"tools":["fs_read","fs_write","execute_bash","mcp_team"],"permission_mode":"restricted","preferred_for":["implementer","Implementation Engineer"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}}],"routing":{"default_assignee":"implementer","rules":[{"id":"route-implementer","match":{"assignee":["implementer"]},"assign_to":"implementer","priority":10}]},"communication":{"protocol":"mcp_inbox","topology":"leader_centered","worker_to_worker":true,"ack_timeout_sec":60,"result_format":"result_envelope_v1","message_store":{"sqlite":".team/runtime/team.db","mirror_files":".team/messages"}},"runtime":{"backend":"tmux","display_backend":"none","session_name":"team-doc-team","auto_launch":true,"require_user_approval_before_launch":true,"max_active_agents":1,"startup_order":["implementer"],"dangerous_auto_approve":false,"fast":false,"tick_interval_sec":2,"push_min_interval_sec":60,"stuck_timeout_sec":300},"context":{"state_file":"team_state.md","artifact_dir":".team/artifacts","log_dir":".team/logs","summarization":{"worker_full_logs":"retain_outside_leader_context","state_update":"after_each_result"}},"tasks":[{"id":"task_initial","title":"Initial document-driven team task","type":"implementation","assignee":"implementer","deps":[],"acceptance":["Worker reports valid result_envelope_v1"],"status":"pending","requires_tools":["mcp_team"],"files":[],"risk":"low"}]}"#;

#[test]
fn compile_base_noprofile_matches_python_dict_order_and_values() {
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_NOPROFILE)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(templated_compact_json(&spec), BASE_NOPROFILE_JSON);
}

#[test]
fn compile_base_returned_spec_passes_validate_spec() {
    // §6 contract: the compiled spec MUST pass model::spec::validate_spec.
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_NOPROFILE)], &[]);
    let spec = compile_team(&team).unwrap();
    let ws = workspace_of(&spec);
    assert!(spec::validate_spec(&spec, Path::new(&ws)).is_ok());
}

#[test]
fn compile_subscription_without_profile_is_thin_manifest() {
    // No profile field → agent has auth_mode=subscription and NO profile / credential_ref keys.
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_NOPROFILE)], &[]);
    let spec = compile_team(&team).unwrap();
    let agent = &spec.get("agents").and_then(Value::as_list).unwrap()[0];
    assert_eq!(agent.get("auth_mode").and_then(Value::as_str), Some("subscription"));
    assert!(agent.get("profile").is_none(), "profile key must be absent");
    assert!(agent.get("credential_ref").is_none(), "credential_ref key must be absent");
}

// Runtime front-matter defaults: every knob from TEAM.md flows through without
// per-role repetition; thin role inherits default_model; leader.model is null
// (TEAM.md has no `model:` key, only `default_model:`).
const RUNTIME_DEFAULTS_TEAM: &str = "\
---
name: doc-team
objective: Compile role docs.
provider: codex
default_model: gpt-5.4
default_auth_mode: subscription
dangerous_auto_approve: true
fast: true
display_backend: ghostty_window
tick_interval_sec: 1
push_min_interval_sec: 3
stuck_timeout_sec: 5
worker_to_worker: true
---

Document-driven team.
";

const RUNTIME_DEFAULTS_ROLE: &str = "\
---
name: implementer
role: Implementation Engineer
provider: codex
tools:
  - fs_read
  - mcp_team
---

Implement bounded tasks.
";

const RUNTIME_DEFAULTS_JSON: &str = r#"{"version":1,"team":{"name":"doc-team","mode":"supervisor_worker","objective":"Compile role docs.","workspace":"__WS__"},"leader":{"id":"leader","role":"leader","provider":"codex","model":null,"tools":["fs_read","fs_list","mcp_team"],"context_policy":{"keep_user_thread":true,"receive_worker_outputs":"business_messages_and_short_summaries","max_worker_result_tokens":2000}},"agents":[{"id":"implementer","role":"Implementation Engineer","provider":"codex","model":"gpt-5.4","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Implement bounded tasks.","file":null},"tools":["fs_read","mcp_team"],"permission_mode":"restricted","preferred_for":["implementer","Implementation Engineer"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}}],"routing":{"default_assignee":"implementer","rules":[{"id":"route-implementer","match":{"assignee":["implementer"]},"assign_to":"implementer","priority":10}]},"communication":{"protocol":"mcp_inbox","topology":"leader_centered","worker_to_worker":true,"ack_timeout_sec":60,"result_format":"result_envelope_v1","message_store":{"sqlite":".team/runtime/team.db","mirror_files":".team/messages"}},"runtime":{"backend":"tmux","display_backend":"ghostty_window","session_name":"team-doc-team","auto_launch":true,"require_user_approval_before_launch":true,"max_active_agents":1,"startup_order":["implementer"],"dangerous_auto_approve":true,"fast":true,"tick_interval_sec":1,"push_min_interval_sec":3,"stuck_timeout_sec":5},"context":{"state_file":"team_state.md","artifact_dir":".team/artifacts","log_dir":".team/logs","summarization":{"worker_full_logs":"retain_outside_leader_context","state_update":"after_each_result"}},"tasks":[{"id":"task_initial","title":"Initial document-driven team task","type":"implementation","assignee":"implementer","deps":[],"acceptance":["Worker reports valid result_envelope_v1"],"status":"pending","requires_tools":["mcp_team"],"files":[],"risk":"low"}]}"#;

#[test]
fn compile_runtime_front_matter_defaults_match_python() {
    let team = build_team(RUNTIME_DEFAULTS_TEAM, &[("implementer.md", RUNTIME_DEFAULTS_ROLE)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(templated_compact_json(&spec), RUNTIME_DEFAULTS_JSON);
}

// provider_models alias ladder: role provider `claude_code` with NO `claude_code`
// key in provider_models falls back to the `claude` key (compiler.py:317-318).
const ALIAS_TEAM: &str = "\
---
name: debate-team
objective: Compile thin role docs.
provider_models:
  claude: claude-sonnet-4-6
default_auth_mode: subscription
display_backend: none
---

Team config.
";

const ALIAS_ROLE: &str = "\
---
name: editor
role: Editor and Defender
provider: claude_code
tools:
  - mcp_team
---

Edit and defend the argument.
";

const ALIAS_JSON: &str = r#"{"version":1,"team":{"name":"debate-team","mode":"supervisor_worker","objective":"Compile thin role docs.","workspace":"__WS__"},"leader":{"id":"leader","role":"leader","provider":"codex","model":null,"tools":["fs_read","fs_list","mcp_team"],"context_policy":{"keep_user_thread":true,"receive_worker_outputs":"business_messages_and_short_summaries","max_worker_result_tokens":2000}},"agents":[{"id":"editor","role":"Editor and Defender","provider":"claude_code","model":"claude-sonnet-4-6","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Edit and defend the argument.","file":null},"tools":["mcp_team"],"permission_mode":"restricted","preferred_for":["editor","Editor and Defender"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}}],"routing":{"default_assignee":"editor","rules":[{"id":"route-editor","match":{"assignee":["editor"]},"assign_to":"editor","priority":10}]},"communication":{"protocol":"mcp_inbox","topology":"leader_centered","worker_to_worker":true,"ack_timeout_sec":60,"result_format":"result_envelope_v1","message_store":{"sqlite":".team/runtime/team.db","mirror_files":".team/messages"}},"runtime":{"backend":"tmux","display_backend":"none","session_name":"team-debate-team","auto_launch":true,"require_user_approval_before_launch":true,"max_active_agents":1,"startup_order":["editor"],"dangerous_auto_approve":false,"fast":false,"tick_interval_sec":2,"push_min_interval_sec":60,"stuck_timeout_sec":300},"context":{"state_file":"team_state.md","artifact_dir":".team/artifacts","log_dir":".team/logs","summarization":{"worker_full_logs":"retain_outside_leader_context","state_update":"after_each_result"}},"tasks":[{"id":"task_initial","title":"Initial document-driven team task","type":"implementation","assignee":"editor","deps":[],"acceptance":["Worker reports valid result_envelope_v1"],"status":"pending","requires_tools":["mcp_team"],"files":[],"risk":"low"}]}"#;

#[test]
fn compile_provider_models_claude_code_falls_back_to_claude_alias() {
    let team = build_team(ALIAS_TEAM, &[("implementer.md", ALIAS_ROLE)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(templated_compact_json(&spec), ALIAS_JSON);
}

// Builtin provider default: no model anywhere → DEFAULT_PROVIDER_MODELS[codex] = gpt-5.5.
const BUILTIN_TEAM: &str = "\
---
name: default-model-team
objective: Compile role docs without model fields.
display_backend: none
---

Team config.
";

const BUILTIN_ROLE: &str = "\
---
name: implementer
role: Implementation Engineer
provider: codex
auth_mode: subscription
tools:
  - mcp_team
---

Implement bounded tasks.
";

const BUILTIN_JSON: &str = r#"{"version":1,"team":{"name":"default-model-team","mode":"supervisor_worker","objective":"Compile role docs without model fields.","workspace":"__WS__"},"leader":{"id":"leader","role":"leader","provider":"codex","model":null,"tools":["fs_read","fs_list","mcp_team"],"context_policy":{"keep_user_thread":true,"receive_worker_outputs":"business_messages_and_short_summaries","max_worker_result_tokens":2000}},"agents":[{"id":"implementer","role":"Implementation Engineer","provider":"codex","model":"gpt-5.5","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Implement bounded tasks.","file":null},"tools":["mcp_team"],"permission_mode":"restricted","preferred_for":["implementer","Implementation Engineer"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}}],"routing":{"default_assignee":"implementer","rules":[{"id":"route-implementer","match":{"assignee":["implementer"]},"assign_to":"implementer","priority":10}]},"communication":{"protocol":"mcp_inbox","topology":"leader_centered","worker_to_worker":true,"ack_timeout_sec":60,"result_format":"result_envelope_v1","message_store":{"sqlite":".team/runtime/team.db","mirror_files":".team/messages"}},"runtime":{"backend":"tmux","display_backend":"none","session_name":"team-default-model-team","auto_launch":true,"require_user_approval_before_launch":true,"max_active_agents":1,"startup_order":["implementer"],"dangerous_auto_approve":false,"fast":false,"tick_interval_sec":2,"push_min_interval_sec":60,"stuck_timeout_sec":300},"context":{"state_file":"team_state.md","artifact_dir":".team/artifacts","log_dir":".team/logs","summarization":{"worker_full_logs":"retain_outside_leader_context","state_update":"after_each_result"}},"tasks":[{"id":"task_initial","title":"Initial document-driven team task","type":"implementation","assignee":"implementer","deps":[],"acceptance":["Worker reports valid result_envelope_v1"],"status":"pending","requires_tools":["mcp_team"],"files":[],"risk":"low"}]}"#;

#[test]
fn compile_subscription_without_model_uses_builtin_provider_default() {
    let team = build_team(BUILTIN_TEAM, &[("implementer.md", BUILTIN_ROLE)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(templated_compact_json(&spec), BUILTIN_JSON);
}

// Two role docs sorted by FILENAME (01-alpha before 02-bravo though names alpha/bravo):
// startup_order, routing rules (one per agent), default_assignee = first agent,
// max_active_agents = min(len, 2).
const TWO_ROLE_A: &str = "\
---
name: alpha
role: Alpha Worker
provider: codex
model: gpt-5.5
auth_mode: subscription
tools:
  - mcp_team
---

Alpha body.
";

const TWO_ROLE_B: &str = "\
---
name: bravo
role: Bravo Worker
provider: codex
model: gpt-5.5
auth_mode: subscription
tools:
  - mcp_team
---

Bravo body.
";

const TWO_AGENTS_JSON: &str = r#"{"version":1,"team":{"name":"doc-team","mode":"supervisor_worker","objective":"Compile role docs.","workspace":"__WS__"},"leader":{"id":"leader","role":"leader","provider":"codex","model":"gpt-5.5","tools":["fs_read","fs_list","mcp_team"],"context_policy":{"keep_user_thread":true,"receive_worker_outputs":"business_messages_and_short_summaries","max_worker_result_tokens":2000}},"agents":[{"id":"alpha","role":"Alpha Worker","provider":"codex","model":"gpt-5.5","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Alpha body.","file":null},"tools":["mcp_team"],"permission_mode":"restricted","preferred_for":["alpha","Alpha Worker"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}},{"id":"bravo","role":"Bravo Worker","provider":"codex","model":"gpt-5.5","auth_mode":"subscription","working_directory":"__WS__","system_prompt":{"inline":"Bravo body.","file":null},"tools":["mcp_team"],"permission_mode":"restricted","preferred_for":["bravo","Bravo Worker"],"avoid_for":[],"output_contract":{"format":"result_envelope_v1","required_fields":["task_id","status","summary","artifacts"]}}],"routing":{"default_assignee":"alpha","rules":[{"id":"route-alpha","match":{"assignee":["alpha"]},"assign_to":"alpha","priority":10},{"id":"route-bravo","match":{"assignee":["bravo"]},"assign_to":"bravo","priority":10}]},"communication":{"protocol":"mcp_inbox","topology":"leader_centered","worker_to_worker":true,"ack_timeout_sec":60,"result_format":"result_envelope_v1","message_store":{"sqlite":".team/runtime/team.db","mirror_files":".team/messages"}},"runtime":{"backend":"tmux","display_backend":"none","session_name":"team-doc-team","auto_launch":true,"require_user_approval_before_launch":true,"max_active_agents":2,"startup_order":["alpha","bravo"],"dangerous_auto_approve":false,"fast":false,"tick_interval_sec":2,"push_min_interval_sec":60,"stuck_timeout_sec":300},"context":{"state_file":"team_state.md","artifact_dir":".team/artifacts","log_dir":".team/logs","summarization":{"worker_full_logs":"retain_outside_leader_context","state_update":"after_each_result"}},"tasks":[{"id":"task_initial","title":"Initial document-driven team task","type":"implementation","assignee":"alpha","deps":[],"acceptance":["Worker reports valid result_envelope_v1"],"status":"pending","requires_tools":["mcp_team"],"files":[],"risk":"low"}]}"#;

#[test]
fn compile_two_agents_sorted_by_filename_with_routing_and_startup_order() {
    // filenames intentionally out of name order to prove sorted(glob) is by filename.
    let team = build_team(TEAM_BASE, &[("02-bravo.md", TWO_ROLE_B), ("01-alpha.md", TWO_ROLE_A)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(templated_compact_json(&spec), TWO_AGENTS_JSON);
}

// ───────────────────────────── compile_team: error paths ─────────────────────────────

const ROLE_MISSING_PROVIDER: &str = "\
---
name: implementer
role: Implementation Engineer
model: gpt-5.5
auth_mode: subscription
tools:
  - mcp_team
---

Implement bounded tasks.
";

#[test]
fn compile_missing_required_provider_field_errors() {
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_MISSING_PROVIDER)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(
        err.to_string().contains("missing front matter field provider"),
        "got: {err}"
    );
}

const ROLE_COMPATIBLE_NO_PROFILE: &str = "\
---
name: implementer
role: Implementation Engineer
provider: codex
model: gpt-5.5
auth_mode: compatible_api
tools:
  - mcp_team
---

Implement bounded tasks.
";

#[test]
fn compile_compatible_api_without_profile_errors() {
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_COMPATIBLE_NO_PROFILE)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("profile is required"), "got: {err}");
}

// ════════════════════════ FIX-LOOP (wave-1) RED tests ════════════════════════
// The first-round fixtures under-specified the contract. These lock the CORRECT
// Python v0.2.11 behavior; each FAILS against the current (too-narrow) impl.
// Golden re-probed via /tmp/probe_fix.py against team-agent-public v0.2.11.

const TM_CODEX: &str = "---\nname: T\nprovider: codex\n---\nx\n";

fn role_nomodel(provider: &str) -> String {
    format!("---\nname: w\nrole: R\nprovider: {provider}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nbody\n")
}

fn agent0(spec: &Value) -> &Value {
    match spec.get("agents") {
        Some(Value::List(items)) if !items.is_empty() => &items[0],
        _ => panic!("spec.agents missing/empty"),
    }
}

fn get_path<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for k in keys {
        cur = cur.get(k)?;
    }
    Some(cur)
}

fn str_path(spec: &Value, keys: &[&str]) -> String {
    get_path(spec, keys)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string at {keys:?}"))
        .to_string()
}

enum AgentsDir<'a> {
    Missing,
    File,
    WithRole(&'a str),
}

/// Build `<base>/<parent>/<leaf>/` with optional TEAM.md and a controllable
/// `agents` entry — lets tests pin `team_dir.parent.name` (A3) and the
/// missing-TEAM.md / missing-agents / agents-is-a-file error paths (A11).
fn build_layout(parent: &str, leaf: &str, team_md: Option<&str>, agents: AgentsDir<'_>) -> PathBuf {
    let team = unique_base().join(parent).join(leaf);
    fs::create_dir_all(&team).unwrap();
    if let Some(md) = team_md {
        fs::write(team.join("TEAM.md"), md).unwrap();
    }
    match agents {
        AgentsDir::Missing => {}
        AgentsDir::File => fs::write(team.join("agents"), "not a dir").unwrap(),
        AgentsDir::WithRole(role) => {
            fs::create_dir_all(team.join("agents")).unwrap();
            fs::write(team.join("agents").join("w.md"), role).unwrap();
        }
    }
    team
}

// ── A1 model resolution ──

#[test]
fn fix_a1_provider_models_precede_default_model() {
    // precedence: provider_models[provider] BEFORE default_model (current swaps them).
    let tm = "---\nname: T\nprovider: codex\ndefault_model: team-y\nprovider_models:\n  codex: pm-z\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(agent0(&spec).get("model").and_then(Value::as_str), Some("pm-z"));
}

#[test]
fn fix_a1_claude_aliases_to_claude_code_provider_models() {
    // TWO-WAY alias: provider `claude` consumes `provider_models[claude_code]`.
    let tm = "---\nname: T\nprovider: codex\nprovider_models:\n  claude_code: cc-v\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("claude"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(agent0(&spec).get("model").and_then(Value::as_str), Some("cc-v"));
}

#[test]
fn fix_a1_builtin_claude_default_is_sonnet_4_6() {
    // DEFAULT_PROVIDER_MODELS: claude / claude_code → "claude-sonnet-4-6" (not -4-5).
    for prov in ["claude", "claude_code"] {
        let team = build_team(TM_CODEX, &[("w.md", &role_nomodel(prov))], &[]);
        let spec = compile_team(&team).unwrap();
        assert_eq!(
            agent0(&spec).get("model").and_then(Value::as_str),
            Some("claude-sonnet-4-6"),
            "provider {prov}"
        );
    }
}

#[test]
fn fix_a1_model_null_when_provider_absent_from_table() {
    // gemini_cli / fake have NO builtin default → model MUST be emitted as null.
    for prov in ["gemini_cli", "fake"] {
        let team = build_team(TM_CODEX, &[("w.md", &role_nomodel(prov))], &[]);
        let spec = compile_team(&team).unwrap();
        assert_eq!(agent0(&spec).get("model"), Some(&Value::Null), "provider {prov} → null");
    }
}

// ── A2 objective ──

#[test]
fn fix_a2_objective_falls_back_to_body() {
    let tm = "---\nname: T\nprovider: codex\n---\nThis is the body.\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["team", "objective"]), "This is the body.");
}

#[test]
fn fix_a2_objective_default_when_no_objective_and_no_body() {
    let tm = "---\nname: T\nprovider: codex\n---\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["team", "objective"]), "Team Agent document-driven team.");
}

// ── A3 name ──

#[test]
fn fix_a3_name_falls_back_to_parent_dir_name() {
    // no `name` → team_dir.parent.name (NOT a hardcoded "team").
    let tm = "---\nprovider: codex\n---\nx\n";
    let team = build_layout("my-parent-dir", "leafteam", Some(tm), AgentsDir::WithRole(&role_nomodel("codex")));
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["team", "name"]), "my-parent-dir");
}

// ── A4 leader.role ──

#[test]
fn fix_a4_leader_role_from_team_meta() {
    let tm = "---\nname: T\nprovider: codex\nleader_role: Captain\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["leader", "role"]), "Captain");
}

// ── A5 session_name + _slug ──

#[test]
fn fix_a5_session_name_slugifies_team_name() {
    // "My Team!" → _slug → "My-Team" → "team-My-Team".
    let tm = "---\nname: My Team!\nprovider: codex\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["runtime", "session_name"]), "team-My-Team");
}

#[test]
fn fix_a5_session_name_override_wins() {
    let tm = "---\nname: My Team!\nprovider: codex\nsession_name: custom-sess\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(&spec, &["runtime", "session_name"]), "custom-sess");
}

// ── A6 tools ──

#[test]
fn fix_a6_tools_shell_maps_to_execute_bash() {
    let role = "---\nname: w\nrole: R\nprovider: codex\nauth_mode: subscription\ntools:\n  - shell\n  - mcp_team\n---\nb\n";
    let team = build_team(TM_CODEX, &[("w.md", role)], &[]);
    let spec = compile_team(&team).expect("shell must normalize to execute_bash and compile");
    assert_eq!(agent0(&spec).get("tools"), Some(&list_str(vec!["execute_bash", "mcp_team"])));
}

#[test]
fn fix_a6_missing_tools_errors() {
    let role = "---\nname: w\nrole: R\nprovider: codex\nauth_mode: subscription\n---\nb\n";
    let team = build_team(TM_CODEX, &[("w.md", role)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("missing front matter field tools"), "got: {err}");
}

#[test]
fn fix_a6_tools_not_a_list_errors() {
    let role = "---\nname: w\nrole: R\nprovider: codex\nauth_mode: subscription\ntools: justastring\n---\nb\n";
    let team = build_team(TM_CODEX, &[("w.md", role)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("tools must be a list"), "got: {err}");
}

// ── A7 system_prompt inline ──

#[test]
fn fix_a7_empty_body_inline_falls_back_to_role() {
    let role = "---\nname: w\nrole: Reviewer Role\nprovider: codex\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n";
    let team = build_team(TM_CODEX, &[("w.md", role)], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(str_path(agent0(&spec), &["system_prompt", "inline"]), "Reviewer Role");
}

// ── A8 auth_mode ──

#[test]
fn fix_a8_official_api_without_profile_errors() {
    // official_api (any non-subscription) without profile MUST NOT silently compile.
    let role = "---\nname: w\nrole: R\nprovider: codex\nauth_mode: official_api\ntools:\n  - mcp_team\n---\nb\n";
    let team = build_team(TM_CODEX, &[("w.md", role)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(
        err.to_string().contains("profile is required when auth_mode is 'official_api'"),
        "got: {err}"
    );
}

#[test]
fn fix_a8_compatible_api_error_names_the_auth_mode() {
    // Full message form (the old test only checked the "profile is required" prefix).
    let team = build_team(TEAM_BASE, &[("implementer.md", ROLE_COMPATIBLE_NO_PROFILE)], &[]);
    let err = compile_team(&team).unwrap_err();
    assert!(
        err.to_string().contains("profile is required when auth_mode is 'compatible_api'"),
        "got: {err}"
    );
}

// ── A9 bool/int coercion (Python bool()/int() over the simple_yaml value) ──

#[test]
fn fix_a9_bool_coercion_yes_and_one_are_true() {
    for v in ["yes", "1"] {
        let tm = format!("---\nname: T\nprovider: codex\ndangerous_auto_approve: {v}\nworker_to_worker: {v}\n---\nx\n");
        let team = build_team(&tm, &[("w.md", &role_nomodel("codex"))], &[]);
        let spec = compile_team(&team).unwrap();
        assert_eq!(get_path(&spec, &["runtime", "dangerous_auto_approve"]), Some(&Value::Bool(true)), "value {v}");
        assert_eq!(get_path(&spec, &["communication", "worker_to_worker"]), Some(&Value::Bool(true)), "value {v}");
    }
}

#[test]
fn fix_a9_bool_coercion_no_is_python_truthy() {
    // FLAG: Python bool("no") is TRUE (only 0/false/False are falsy). The contract
    // note said "no->false" but Python golden is no->true; we lock Python.
    let tm = "---\nname: T\nprovider: codex\ndangerous_auto_approve: no\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(get_path(&spec, &["runtime", "dangerous_auto_approve"]), Some(&Value::Bool(true)));
}

#[test]
fn fix_a9_int_coercion_of_quoted_string() {
    // tick_interval_sec: "5" (a YAML string) → int("5") = 5.
    let tm = "---\nname: T\nprovider: codex\ntick_interval_sec: \"5\"\n---\nx\n";
    let team = build_team(tm, &[("w.md", &role_nomodel("codex"))], &[]);
    let spec = compile_team(&team).unwrap();
    assert_eq!(get_path(&spec, &["runtime", "tick_interval_sec"]), Some(&Value::Int(5)));
}

// ── A10 CRLF normalization ──

#[test]
fn fix_a10_crlf_role_doc_front_matter_is_parsed() {
    // Windows-authored \r\n doc: read_text universal-newlines normalizes before
    // the "---\n" check, so the front matter parses (current keeps \r\n → no FM).
    let crlf_role = "---\r\nname: crlfworker\r\nrole: R\r\nprovider: codex\r\nauth_mode: subscription\r\ntools:\r\n  - mcp_team\r\n---\r\n\r\nbody\r\n";
    let team = build_team(TM_CODEX, &[("w.md", crlf_role)], &[]);
    let spec = compile_team(&team).expect("CRLF role doc must parse its front matter");
    assert_eq!(agent0(&spec).get("id").and_then(Value::as_str), Some("crlfworker"));
    assert_eq!(str_path(agent0(&spec), &["system_prompt", "inline"]), "body");
}

// ── A11 error-message paths ──

#[test]
fn fix_a11_missing_team_md_message_includes_team_md_path() {
    let team = build_layout("p", "teamdir", None, AgentsDir::WithRole(&role_nomodel("codex")));
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("/TEAM.md: missing TEAM.md"), "got: {err}");
}

#[test]
fn fix_a11_missing_agents_dir_message_includes_agents_path() {
    let team = build_layout("p", "teamdir", Some(TM_CODEX), AgentsDir::Missing);
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("/agents: missing agents directory"), "got: {err}");
}

#[test]
fn fix_a11_agents_is_a_file_reports_no_role_docs() {
    // Python uses exists() (not is_dir()): agents-as-a-file passes the existence
    // gate, then the empty glob → "no role docs found" (path = agents dir).
    let team = build_layout("p", "teamdir", Some(TM_CODEX), AgentsDir::File);
    let err = compile_team(&team).unwrap_err();
    assert!(err.to_string().contains("/agents: no role docs found"), "got: {err}");
}
