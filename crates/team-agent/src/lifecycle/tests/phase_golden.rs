use super::launch_spawn::seed_healthy_coordinator;
use super::*;
use crate::cli::{
    cmd_collect, cmd_send, cmd_status, lifecycle_port, CmdOutput, CmdResult, CollectArgs, SendArgs,
    StatusArgs,
};
use crate::transport::test_support::OfflineTransport;
use crate::transport::WindowName;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const ANCESTRY_ENV: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const CALLER_IDENTITY_ENVS: &[&str] = &[
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
];

#[test]
#[serial_test::serial(env)]
fn phase_b_golden_events_state_status_zero_drift() {
    let baseline = phase_fixture_path("phase_b").join("golden.json");
    let actual = run_phase_golden(PhaseGolden {
        phase: "phase_b",
        team_key: "teamdir",
        lifecycle_op: phase_b_reset_discard_session,
    });
    if std::env::var_os("TEAM_AGENT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline parent")).unwrap();
        std::fs::write(&baseline, pretty(&actual)).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&baseline)
        .unwrap_or_else(|e| panic!("missing golden baseline {}: {e}", baseline.display()));
    let expected: Value = serde_json::from_str(&expected).expect("parse golden baseline");
    assert_eq!(
        actual,
        expected,
        "phase B golden drift; update intentionally with TEAM_AGENT_UPDATE_GOLDEN=1 only after review"
    );
}

#[test]
#[serial_test::serial(env)]
fn phase_c_golden_events_state_status_zero_drift() {
    let baseline = phase_fixture_path("phase_c").join("golden.json");
    let actual = run_phase_golden(PhaseGolden {
        phase: "phase_c",
        team_key: "teamdir",
        lifecycle_op: phase_b_reset_discard_session,
    });
    if std::env::var_os("TEAM_AGENT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline parent")).unwrap();
        std::fs::write(&baseline, pretty(&actual)).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&baseline)
        .unwrap_or_else(|e| panic!("missing golden baseline {}: {e}", baseline.display()));
    let expected: Value = serde_json::from_str(&expected).expect("parse golden baseline");
    assert_eq!(
        actual,
        expected,
        "phase C golden drift; update intentionally with TEAM_AGENT_UPDATE_GOLDEN=1 only after review"
    );
}

#[test]
#[serial_test::serial(env)]
fn phase_d_golden_events_state_status_zero_drift() {
    let baseline = phase_fixture_path("phase_d").join("golden.json");
    let actual = run_phase_golden(PhaseGolden {
        phase: "phase_d",
        team_key: "teamdir",
        lifecycle_op: phase_b_reset_discard_session,
    });
    if std::env::var_os("TEAM_AGENT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline parent")).unwrap();
        std::fs::write(&baseline, pretty(&actual)).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&baseline)
        .unwrap_or_else(|e| panic!("missing golden baseline {}: {e}", baseline.display()));
    let expected: Value = serde_json::from_str(&expected).expect("parse golden baseline");
    assert_eq!(
        actual,
        expected,
        "phase D golden drift; update intentionally with TEAM_AGENT_UPDATE_GOLDEN=1 only after review"
    );
}

#[test]
#[serial_test::serial(env)]
fn phase_e_golden_events_state_status_zero_drift() {
    let baseline = phase_fixture_path("phase_e").join("golden.json");
    let actual = run_phase_golden(PhaseGolden {
        phase: "phase_e",
        team_key: "teamdir",
        lifecycle_op: phase_b_reset_discard_session,
    });
    if std::env::var_os("TEAM_AGENT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline parent")).unwrap();
        std::fs::write(&baseline, pretty(&actual)).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&baseline)
        .unwrap_or_else(|e| panic!("missing golden baseline {}: {e}", baseline.display()));
    let expected: Value = serde_json::from_str(&expected).expect("parse golden baseline");
    assert_eq!(
        actual,
        expected,
        "phase E golden drift; update intentionally with TEAM_AGENT_UPDATE_GOLDEN=1 only after review"
    );
}

#[test]
#[serial_test::serial(env)]
fn phase_f_golden_events_state_status_zero_drift() {
    let baseline = phase_fixture_path("phase_f").join("golden.json");
    let actual = run_phase_golden(PhaseGolden {
        phase: "phase_f",
        team_key: "teamdir",
        lifecycle_op: phase_b_reset_discard_session,
    });
    if std::env::var_os("TEAM_AGENT_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(baseline.parent().expect("baseline parent")).unwrap();
        std::fs::write(&baseline, pretty(&actual)).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&baseline)
        .unwrap_or_else(|e| panic!("missing golden baseline {}: {e}", baseline.display()));
    let expected: Value = serde_json::from_str(&expected).expect("parse golden baseline");
    assert_eq!(
        actual,
        expected,
        "phase F golden drift; update intentionally with TEAM_AGENT_UPDATE_GOLDEN=1 only after review"
    );
}

#[derive(Clone, Copy)]
struct PhaseGolden {
    phase: &'static str,
    team_key: &'static str,
    lifecycle_op: fn(&Path, &OfflineTransport, &'static str) -> Value,
}

fn run_phase_golden(spec: PhaseGolden) -> Value {
    let _permission_mode = EnvVarGuard::set(ANCESTRY_ENV, "[]");
    let _caller_identity = CALLER_IDENTITY_ENVS
        .iter()
        .map(|key| EnvVarGuard::unset(key))
        .collect::<Vec<_>>();
    let team = two_worker_team_dir();
    let workspace = team.parent().expect("workspace").to_path_buf();
    seed_healthy_coordinator(&workspace);
    let launch_transport = codex_ready_transport();
    let quick_start = quick_start_with_transport_in_workspace_with_display(
        &workspace,
        &team,
        None,
        true,
        None,
        &launch_transport,
        false,
    );
    let status_compact = cmd_status(&StatusArgs {
        agent: None,
        workspace: workspace.clone(),
        detail: false,
        summary: false,
        json: true,
        team: Some(spec.team_key.to_string()),
    });
    let status_detail = cmd_status(&StatusArgs {
        agent: None,
        workspace: workspace.clone(),
        detail: true,
        summary: false,
        json: true,
        team: Some(spec.team_key.to_string()),
    });
    let send = cmd_send(&SendArgs {
        target: Some("w1".to_string()),
        message: vec!["phase".to_string(), "golden".to_string()],
        targets: None,
        workspace: workspace.clone(),
        team: Some(spec.team_key.to_string()),
        task: None,
        sender: "leader".to_string(),
        no_ack: true,
        no_wait: true,
        watch_result: false,
        timeout: 0.1,
        confirm_human: false,
        json: true,
        message_id: Some("phase-golden-message".to_string()),
        pane: None,
        to_name: None,
    });
    let collect = cmd_collect(&CollectArgs {
        workspace: workspace.clone(),
        result_file: None,
        json: true,
        team: Some(spec.team_key.to_string()),
    });
    let lifecycle_transport = codex_ready_transport()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1"), WindowName::new("w2")]);
    let lifecycle = (spec.lifecycle_op)(&workspace, &lifecycle_transport, spec.team_key);
    let shutdown = lifecycle_port::shutdown_with_transport(
        &workspace,
        true,
        Some(spec.team_key),
        &lifecycle_transport,
    );

    let raw = json!({
        "phase": spec.phase,
        "script": [
            {
                "step": "quick-start",
                "exit_code": if quick_start.is_ok() { 0 } else { 1 },
                "output": quick_start_value(quick_start),
            },
            { "step": "status-json", "result": cmd_value(status_compact) },
            { "step": "status-detail-json", "result": cmd_value(status_detail) },
            { "step": "send", "result": cmd_value(send) },
            { "step": "collect", "result": cmd_value(collect) },
            { "step": "phase-lifecycle-op", "output": lifecycle },
            {
                "step": "shutdown-keep-logs",
                "exit_code": if shutdown.is_ok() { 0 } else { 1 },
                "output": match shutdown {
                    Ok(value) => value,
                    Err(error) => json!({"ok": false, "error": error.to_string()}),
                },
            },
        ],
        "events_jsonl": read_events(&workspace),
        "state_json": read_state(&workspace),
        "transport": {
            "spawns": lifecycle_transport.spawn_window_records(),
        },
    });

    let mut ctx = NormalizeCtx::new(&workspace);
    normalize_value(raw, &mut ctx, None)
}

fn phase_b_reset_discard_session(
    workspace: &Path,
    transport: &OfflineTransport,
    team_key: &'static str,
) -> Value {
    match crate::lifecycle::reset_agent_with_transport(
        workspace,
        &aid("w1"),
        true,
        false,
        Some(team_key),
        transport,
    ) {
        Ok(ResetAgentOutcome::Reset {
            start_mode,
            discarded_session_id,
            session_id,
            new_session_id,
            ..
        }) => json!({
            "ok": true,
            "operation": "reset-agent --discard-session",
            "status": "running",
            "start_mode": format!("{start_mode:?}"),
            "discarded_session_id": discarded_session_id.map(|id| id.as_str().to_string()),
            "session_id": session_id.map(|id| id.as_str().to_string()),
            "new_session_id": new_session_id.map(|id| id.as_str().to_string()),
        }),
        Ok(other) => json!({
            "ok": true,
            "operation": "reset-agent --discard-session",
            "status": format!("{other:?}"),
        }),
        Err(error) => json!({
            "ok": false,
            "operation": "reset-agent --discard-session",
            "error": error.to_string(),
        }),
    }
}

fn two_worker_team_dir() -> PathBuf {
    let team = temp_ws().join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: phasegolden\nobjective: Phase golden.\nprovider: codex\n---\n\nPhase golden team.\n",
    )
    .unwrap();
    for id in ["w1", "w2"] {
        std::fs::write(team.join("agents").join(format!("{id}.md")), role_doc(id)).unwrap();
    }
    team
}

fn codex_ready_transport() -> OfflineTransport {
    let mut transport = OfflineTransport::new();
    for pane in 0..16 {
        transport = transport.with_capture_for_pane(format!("%{pane}"), "OpenAI Codex");
    }
    transport
}

fn role_doc(id: &str) -> String {
    format!(
        "---\nname: {id}\nrole: {id} Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{id} worker.\n"
    )
}

fn quick_start_value(result: Result<QuickStartReport, LifecycleError>) -> Value {
    match result {
        Ok(QuickStartReport::Ready {
            session_name,
            launch,
            display_backend,
            worker_readiness,
            ..
        }) => {
            let agents = launch
                .started
                .iter()
                .map(|started| {
                    json!({
                        "agent_id": started.agent_id.as_str(),
                        "target": started.target,
                        "start_mode": format!("{:?}", started.start_mode),
                        "session_id": started.session_id.as_ref().map(|id| id.as_str().to_string()),
                        "rollout_path": started.rollout_path.as_ref().map(|path| path.as_path().to_string_lossy().to_string()),
                        "layout_window": started.layout_window.as_ref().map(|window| window.as_str().to_string()),
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "ok": true,
                "status": "ready",
                "session_name": session_name.as_str(),
                "display_backend": display_backend,
                "worker_readiness": format!("{worker_readiness:?}"),
                "started": agents,
            })
        }
        Ok(other) => json!({"ok": false, "status": format!("{other:?}")}),
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    }
}

fn cmd_value(result: Result<CmdResult, crate::cli::CliError>) -> Value {
    match result {
        Ok(cmd) => json!({
            "exit_code": cmd.exit.code(),
            "output": match cmd.output {
                CmdOutput::Json(value) => value,
                CmdOutput::Human(text) => json!({"human": text}),
                CmdOutput::None => Value::Null,
            },
        }),
        Err(error) => json!({
            "exit_code": 1,
            "error": error.to_string(),
        }),
    }
}

fn read_events(workspace: &Path) -> Vec<Value> {
    let path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn read_state(workspace: &Path) -> Value {
    crate::state::persist::load_runtime_state(workspace).unwrap_or_else(|_| json!(null))
}

fn phase_fixture_path(phase: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("0_5_0_phase_golden")
        .join(phase)
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

struct NormalizeCtx {
    workspace_aliases: Vec<String>,
    temp_aliases: Vec<String>,
    pane_ids: BTreeMap<String, String>,
}

impl NormalizeCtx {
    fn new(workspace: &Path) -> Self {
        Self {
            workspace_aliases: path_aliases(workspace),
            temp_aliases: path_aliases(&std::env::temp_dir()),
            pane_ids: BTreeMap::new(),
        }
    }

    fn pane_token(&mut self, pane: &str) -> String {
        if let Some(token) = self.pane_ids.get(pane) {
            return token.clone();
        }
        let token = format!("<PANE:{}>", self.pane_ids.len());
        self.pane_ids.insert(pane.to_string(), token.clone());
        token
    }
}

fn path_aliases(path: &Path) -> Vec<String> {
    let mut aliases = Vec::new();
    push_path_alias(&mut aliases, path.to_path_buf());
    if let Ok(canonical) = std::fs::canonicalize(path) {
        push_path_alias(&mut aliases, canonical);
    }
    aliases.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    aliases.dedup();
    aliases
}

fn push_path_alias(aliases: &mut Vec<String>, path: PathBuf) {
    let path = path.to_string_lossy().to_string();
    if path.is_empty() {
        return;
    }
    aliases.push(path.clone());
    if let Some(stripped) = path.strip_prefix("/private/") {
        aliases.push(format!("/{stripped}"));
    } else if path.starts_with('/') {
        aliases.push(format!("/private{path}"));
    }
}

fn normalize_value(value: Value, ctx: &mut NormalizeCtx, key: Option<&str>) -> Value {
    if matches!(key, Some("env_overlay_keys" | "env_unset_keys")) {
        return json!("<ENV_KEYS>");
    }
    match value {
        Value::Object(map) => {
            let sorted = map.into_iter().collect::<BTreeMap<_, _>>();
            let mut out = Map::new();
            for (child_key, child) in sorted {
                out.insert(
                    child_key.clone(),
                    normalize_value(child, ctx, Some(child_key.as_str())),
                );
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalize_value(item, ctx, key))
                .collect(),
        ),
        Value::String(text) => normalize_string(text, ctx, key),
        Value::Number(number) if key_is_pid_or_duration(key) => {
            if number.is_u64() || number.is_i64() {
                json!(0)
            } else {
                Value::Number(number)
            }
        }
        other => other,
    }
}

fn normalize_string(text: String, ctx: &mut NormalizeCtx, key: Option<&str>) -> Value {
    let key = key.unwrap_or_default();
    if key_is_timestamp(key) {
        return json!("<TS>");
    }
    if key.contains("endpoint") {
        return json!("<SOCKET>");
    }
    if key_is_id(key, &text) {
        return json!("<ID>");
    }
    if key_contains_path(key) {
        return json!(normalize_path_string(&text, ctx));
    }
    if text.starts_with('%') && text[1..].chars().all(|c| c.is_ascii_digit()) {
        return json!(ctx.pane_token(&text));
    }
    let normalized = normalize_path_string(&text, ctx);
    if normalized != text {
        return json!(normalized);
    }
    json!(text)
}

fn normalize_path_string(text: &str, ctx: &NormalizeCtx) -> String {
    let mut out = text.to_string();
    for alias in &ctx.workspace_aliases {
        out = out.replace(alias, "<WORKSPACE>");
    }
    out = out.replace("/private<WORKSPACE>", "<WORKSPACE>");
    for alias in &ctx.temp_aliases {
        out = out.replace(alias, "<TMP>");
    }
    out = normalize_team_agent_binary_path(&out);
    out = normalize_tmux_socket_dir(&out);
    normalize_socket_token(&out)
}

fn normalize_team_agent_binary_path(text: &str) -> String {
    let mut out = text.to_string();
    // 0.5.0 hermetic 教训「环境路径类 token 化」的直接延伸
    // (leader msg_6ee04cf5aee8):归一化改为结构判据,不绑路径前缀。
    // 用户 CARGO_TARGET_DIR 可以指到任意目录(默认 `<repo>/target`,
    // 全局改道时是 `/Volumes/nvme/cargo-target`,CI 时可能是别的)。
    // 结构判据:任意绝对路径下的 `/deps/team_agent-<hex>`(可选 `.exe`)
    // → `<TEAM_AGENT_BIN>`。同时保留旧 marker 兼容(处理旧固定字符串
    // 出现在 output 里,以及 `debug/team-agent` / `release/team-agent`
    // 这两个非 `/deps/` 分支)。
    out = normalize_deps_team_agent_by_structure(&out);
    for marker in [
        "/target/debug/deps/team_agent-",
        "/target/debug/team-agent",
        "/target/release/team-agent",
    ] {
        out = replace_path_with_marker(out, marker, "<TEAM_AGENT_BIN>");
    }
    out
}

/// Structural (path-prefix-independent) normalization for
/// `/…/deps/team_agent-<hex>(.exe)?` occurrences. Scans the input for
/// every `/deps/team_agent-` marker and rewrites the containing
/// absolute path token to `<TEAM_AGENT_BIN>`, no matter which
/// `CARGO_TARGET_DIR` produced it.
fn normalize_deps_team_agent_by_structure(text: &str) -> String {
    const MARKER: &str = "/deps/team_agent-";
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(rel) = text[i..].find(MARKER) else {
            out.push_str(&text[i..]);
            break;
        };
        let marker_idx = i + rel;
        // Locate the start of the absolute path token: walk back from
        // `marker_idx` while the byte is not one of a small set of
        // delimiters (quote, whitespace, `[`, `(`, `,`, `:` after
        // whitespace). The absolute-path anchor MUST also start with
        // `/` — if the containing token is not absolute, skip
        // rewriting so we don't eat unrelated text.
        let path_start = text[..marker_idx]
            .rfind(|c: char| matches!(c, '"' | '\'' | ' ' | '[' | '(' | ',' | '\n' | '\t'))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        // Confirm the token starts with `/` (absolute path).
        if !text[path_start..].starts_with('/') {
            // Not an absolute path — leave alone.
            out.push_str(&text[i..marker_idx + MARKER.len()]);
            i = marker_idx + MARKER.len();
            continue;
        }
        // Extend the end past the hex hash + optional `.exe`. Same
        // continuation rule as `replace_path_with_marker`: alnum + `-`
        // + `_` + `.`.
        let mut path_end = marker_idx + MARKER.len();
        while path_end < text.len() {
            let ch = text[path_end..].chars().next().expect("char at boundary");
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                path_end += ch.len_utf8();
            } else {
                break;
            }
        }
        // Emit everything up to the path start, then the token, then
        // continue past the eaten span.
        out.push_str(&text[i..path_start]);
        out.push_str("<TEAM_AGENT_BIN>");
        i = path_end;
    }
    out
}

fn replace_path_with_marker(mut text: String, marker: &str, token: &str) -> String {
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find(marker) {
        let marker_idx = search_from + offset;
        let start = text[..marker_idx]
            .rfind(|c: char| matches!(c, '"' | '\'' | ' ' | '[' | '('))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let mut end = marker_idx + marker.len();
        while end < text.len() {
            let ch = text[end..].chars().next().expect("char at boundary");
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                end += ch.len_utf8();
            } else {
                break;
            }
        }
        text.replace_range(start..end, token);
        search_from = start + token.len();
    }
    text
}

fn normalize_tmux_socket_dir(text: &str) -> String {
    let out = text
        .replace("/private/tmp/tmux-", "<TMP>/tmux-")
        .replace("/tmp/tmux-", "<TMP>/tmux-");
    normalize_tmux_uid_with_prefix(out, "<TMP>/tmux-")
}

fn normalize_tmux_uid_with_prefix(mut text: String, prefix: &str) -> String {
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find(prefix) {
        let start = search_from + offset + prefix.len();
        let mut end = start;
        while end < text.len() {
            let ch = text[end..].chars().next().expect("char at boundary");
            if ch.is_ascii_digit() {
                end += ch.len_utf8();
            } else {
                break;
            }
        }
        if end == start {
            search_from = start;
            continue;
        }
        text.replace_range(start..end, "<UID>");
        search_from = start + "<UID>".len();
    }
    text
}

fn normalize_socket_token(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(idx) = out.find("ta-") {
        let end = out[idx..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .map(|offset| idx + offset)
            .unwrap_or_else(|| out.len());
        if end == idx + 3 {
            break;
        }
        out.replace_range(idx..end, "ta-<SOCKET>");
    }
    out
}

fn key_is_timestamp(key: &str) -> bool {
    key == "ts" || key.ends_with("_at") || key.contains("timestamp")
}

fn key_is_pid_or_duration(key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    key.contains("pid")
        || key.ends_with("_ms")
        || key.contains("duration")
        || key.contains("elapsed")
}

fn key_contains_path(key: &str) -> bool {
    key.contains("path")
        || key.contains("workspace")
        || key.contains("file")
        || key.contains("socket")
        || key.contains("endpoint")
        || key == "log"
}

fn key_is_id(key: &str, text: &str) -> bool {
    key == "message_id"
        || key == "result_id"
        || key == "watcher_id"
        || key.ends_with("_message_id")
        || key.ends_with("_result_id")
        || text.starts_with("msg_")
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}
