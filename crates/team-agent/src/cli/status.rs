//! cli · status — 五行 triage summary 渲染 + agent 分类计数(commands.py
//! `_format_status_summary` / `_agent_summary_counts` / `_interaction_counts` /
//! `_latest_result_line` + `cmd_status` 的 compact 不变量)。

use super::*;

/// 单 agent 双信号(`status` × `agent_health.status`)→ 桶分类(`_agent_summary_counts` 内层)。
/// **穷尽 match,禁默认 idle 臂**(§11)。`raw`/`hstatus` 已 lowercase。
pub fn classify_agent_bucket(raw_status: &str, health_status: &str) -> SummaryBucket {
    let raw = raw_status.to_ascii_lowercase();
    let health = health_status.to_ascii_lowercase();
    if matches!(raw.as_str(), "failed" | "error") || matches!(health.as_str(), "failed" | "error")
    {
        SummaryBucket::Failed
    } else if matches!(raw.as_str(), "stopped" | "done") || health == "done" {
        SummaryBucket::Stopped
    } else if raw == "busy" || matches!(health.as_str(), "running" | "working") {
        SummaryBucket::Busy
    } else if health == "idle" {
        SummaryBucket::Idle
    } else if raw == "running" {
        SummaryBucket::Running
    } else {
        SummaryBucket::Unknown
    }
}

/// `_agent_summary_counts`(`commands.py:309-330`):遍历 agents×health → [`SummaryCounts`]。
pub fn agent_summary_counts(agents: &Value, health: &Value) -> SummaryCounts {
    let mut counts = SummaryCounts::default();
    let Some(agent_map) = agents.as_object() else {
        return counts;
    };
    for (agent_id, agent) in agent_map {
        let raw = agent
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        let hstatus = health
            .get(agent_id)
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match classify_agent_bucket(raw, hstatus) {
            SummaryBucket::Running => counts.running += 1,
            SummaryBucket::Busy => counts.busy += 1,
            SummaryBucket::Idle => counts.idle += 1,
            SummaryBucket::Stopped => counts.stopped += 1,
            SummaryBucket::Failed => counts.failed += 1,
            SummaryBucket::Unknown => counts.unknown += 1,
        }
    }
    counts
}

/// `_interaction_counts`(`commands.py:292-306`):遍历 agents 的 `interacted` 字段。
pub fn interaction_counts(agents: &Value) -> InteractionCounts {
    let mut counts = InteractionCounts::default();
    let Some(agent_map) = agents.as_object() else {
        return counts;
    };
    for agent in agent_map.values() {
        let interacted = agent
            .get("interacted")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !interacted.is_empty() && interacted != "never" {
            counts.interacted += 1;
        } else {
            counts.never += 1;
        }
    }
    counts
}

/// `_format_status_summary`(`commands.py:263-289`):把 status `Value` 渲染成五行 triage 文本。
/// **Gap 18a 字节锁(§11)**:line[2] 精确串
/// `agents: N — running=.. busy=.. idle=.. stopped=.. failed=.. unknown=..`,
/// `(N interacted, M never)` 仅当 interacted>0 才追加(`commands.py:280-282`)。
/// 五行:coordinator / receiver / agents / queued / latest result。空格/破折号/顺序禁改。
pub fn format_status_summary(data: &Value) -> String {
    let coordinator = non_empty_str(
        data.get("coordinator")
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str),
        "stopped",
    );
    let schema_ok = python_truthy(
        data
        .get("coordinator")
        .and_then(|v| v.get("schema_ok"))
            .unwrap_or(&Value::Null),
    );
    let tmux = python_truthy(data.get("tmux_session_present").unwrap_or(&Value::Null));
    let receiver = data.get("leader_receiver").unwrap_or(&Value::Null);
    let pane = non_empty_str(
        receiver.get("pane_id").and_then(Value::as_str),
        "-",
    );
    let cmd = first_non_empty_str(
        &[
            receiver.get("pane_current_command").and_then(Value::as_str),
            receiver.get("current_command").and_then(Value::as_str),
        ],
        "-",
    );
    let agents = data.get("agents").unwrap_or(&Value::Null);
    let health = data.get("agent_health").unwrap_or(&Value::Null);
    let counts = agent_summary_counts(agents, health);
    let interactions = interaction_counts(agents);
    let mut agent_line = format!(
        "agents: {} — running={} busy={} idle={} stopped={} failed={} unknown={}",
        counts.total(),
        counts.running,
        counts.busy,
        counts.idle,
        counts.stopped,
        counts.failed,
        counts.unknown
    );
    if interactions.interacted > 0 {
        agent_line.push_str(&format!(
            " ({} interacted, {} never)",
            interactions.interacted, interactions.never
        ));
    }
    let queued = data
        .get("queued_messages")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let latest = data
        .get("latest_results")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .filter(|v| python_truthy(v))
        .map(format_latest_result)
        .unwrap_or_else(|| "none".to_string());
    format!(
        "coordinator: {coordinator} schema_ok={schema_ok} tmux={tmux}\nreceiver: {pane} cmd={cmd}\n{agent_line}\nqueued: {queued} mailbox messages awaiting delivery\nlatest result: {latest}"
    )
}

/// `_latest_result_line`(`commands.py:333-337`):agent_id/summary/created_at 渲染单行。
/// summary `\n`→` ` 后 [:80] 截断、空 → `-`;agent_id 空 → `-`;created_at 经 age_text。
pub(crate) fn format_latest_result(value: &Value) -> String {
    let agent = non_empty_str(
        value
        .get("agent_id")
        .and_then(Value::as_str)
        ,
        "-",
    );
    let raw_summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("");
    let summary_flat = raw_summary.replace('\n', " ");
    let summary = prefix_chars(&summary_flat, 80);
    let summary = if summary.is_empty() {
        "-".to_string()
    } else {
        summary
    };
    let created = age_text(
        value
        .get("created_at")
        .and_then(Value::as_str)
        ,
    );
    format!("{agent} -> {summary} @ {created}")
}

/// `cmd_status` 的 `--json` 分支 CLI **独占**的不变量(`commands.py:99`):
/// `compact = not (detail is True)`。即 `detail=false ⇒ compact=true`(默认压缩),
/// `detail=true ⇒ compact=false`(全量)。这是 CLI 在 status 委派之上唯一拥有的字节级映射。
pub fn status_compact_flag(detail: bool) -> bool {
    !detail
}
