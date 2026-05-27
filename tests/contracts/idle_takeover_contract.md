# Idle Takeover Contract

Gap 32 replaces screen-scraped idle detection with provider session-file facts.
This contract is the outside behavior required by the idle/takeover redesign.
Implementation details are intentionally excluded except where the architecture
requires provider-neutral module boundaries.

## Public Contract Surface

The acceptance tests exercise an injectable public surface:

```python
from team_agent.idle_takeover import (
    classify_provider_turn_state,
    evaluate_takeover_reminder,
    record_turn_open_after_delivery,
    process_abnormal_records,
    detect_whole_team_gone,
)
from team_agent.provider_state.registry import get_provider_registry
```

`classify_provider_turn_state(provider, session_log_text, *, process=None,
file_silence_seconds=0, registry=None, event_sink=None) -> dict` returns:

```python
{
    "state": "idle" | "working" | "idle_interrupted" | "blocked_on_human" | "abnormal" | "unknown",
    "turn_id": str | None,
    "reason": str,
    "source": str,
    "annotations": list[str],
    "diagnostics": list[dict],
}
```

`evaluate_takeover_reminder(nodes, *, monitor_state, now_monotonic,
debounce_seconds, suspend_intervals=None, event_sink=None) -> dict` returns:

```python
{
    "should_ping": bool,
    "message": str | None,
    "reason": str,
    "annotations": list[dict],
    "monitor_state": dict,
}
```

`record_turn_open_after_delivery(monitor_state, *, node_id, turn_id,
delivered_message_id, now_monotonic, event_sink=None) -> dict` records the real
turn-open edge that re-arms a previously acknowledged idle reminder.

`process_abnormal_records(records, *, registry, notification_state,
event_sink=None) -> dict` emits abnormal notifications and discovery-log entries.

`detect_whole_team_gone(snapshot, *, marker_store, event_sink=None) -> dict`
detects whole-team disappearance independently of the coordinator process.

The returned dictionaries may carry additional fields, but these fields and
enum values are stable contract.

## Provider Session Facts

Provider readers consume session log text, not pane screens.

Codex rollout JSONL real markers:
- `event_msg.payload.type == "task_started"` means an open turn exists.
- `event_msg.payload.type == "task_complete"` closes that turn as `idle`.
- `event_msg.payload.type == "turn_aborted"` with `reason == "interrupted"`
  closes that turn as `idle_interrupted`.

Claude transcript JSONL real markers:
- assistant `message.stop_reason == "tool_use"` means `working`.
- assistant `message.stop_reason == "end_turn"` means `idle`.
- user text exactly `[Request interrupted by user]` means `idle_interrupted`.
- user `tool_result` with `is_error == true` is a structured tool error fact.
- system `subtype == "api_error"` and `level == "error"` is a provider API
  error fact.
- trailing metadata records such as `stop_hook_summary`, `turn_duration`,
  `last-prompt`, `ai-title`, and `permission-mode` must be ignored when finding
  the last turn-lifecycle verdict.

Codex failed and permission-request fixtures are schema-derived in this test
slice because no live local archive had those records. They are derived from
the codex-rs app-server and hook schemas cited in the research artifact. Mac
mini E2E must later supplement them with real Codex captures.

## C1. Armed Only After Delegation

The idle watcher fires only after at least one worker has opened at least one
turn since the last ack/re-arm. Pure leader/user idleness with no delegated
worker turn never pings. Delegated work that has returned to all-idle for the
debounce window does ping.

## C2. Neutral Ping Wording

The leader ping is an all-idle checkpoint. It may say all nodes are idle and
offer `acknowledge-idle`, but it must not assert that unfinished work exists or
that a task was dropped.

## C3. Re-Arm On Turn Open

Suppression from `acknowledge-idle` clears only when a provider turn-open event
is observed. A delivered inbound message must reliably produce that turn-open
edge; delivered-but-unprocessed work cannot leave the watcher permanently
suppressed.

## C4. PID Identity Liveness

The process guard checks provider process identity, not only PID existence.
If a session log has an open turn and the recorded PID has been replaced by a
different start time or command identity, the node is `abnormal` with
`crashed_mid_turn`, never `working`.

## C5. Fail Safe On Unknown Format

Malformed, unreadable, or changed provider session format returns `unknown`,
emits diagnostics, and is not considered idle. The all-idle predicate never
fires while any node is `unknown`.

## C6. Provider Strings Stay In Provider Modules

Provider-specific knowledge is confined to `provider_state/<provider>.py` and
registry data. Neutral modules `idle_predicate`, `abnormal_track`, and `wake`
contain no provider names such as `codex` or `claude`.

## C7. Registry Is Shipped Infra Data

Per-CLI registry entries for file locations, event types, and error lists are
shipped with the runtime as infra data. They are not user-mandatory config and
they are not encoded in neutral predicate logic.

## C8. Abnormal Notification Dedup

Abnormal notifications dedupe by `(error_signature, turn_id)`. A retry loop that
repeats the same provider error in the same turn emits one notification.

## C9. Catch-Bias Scope

Default notify applies only to provider-structured error or failed-class
records that do not match known white/black lists. Arbitrary unrecognized lines
or changed formats become `unknown`/diagnostic, not default abnormal
notifications. Raw structured error records are attached to the notification and
discovery log.

## C10. Whole-Team Gone Is Coordinator-Independent

Whole-team-gone detection does not depend on the coordinator still running. An
independent path reads PID identity and durable markers. Clean shutdown and
restart-in-progress are silent; unexpected disappearance records a durable
marker and defers user escalation until the next leader command as a last resort.

## C11. Suspend Time Does Not Count

Debounce uses monotonic active time. System sleep/suspend intervals are excluded
from the all-idle elapsed duration. On wake, the watcher re-evaluates instead of
counting suspended time as idle.

## C12. Interrupted Counts As Idle With Annotation

`idle_interrupted` nodes count as idle for the all-idle predicate, but the ping
annotates which nodes were interrupted.

## C13. Leader Is A Provider Node

The leader is read through its own provider transcript with the same reader
interface. Leader idle means the leader's own provider turn has closed. Leader
process disappearance routes to whole-team-gone, not worker
`crashed_mid_turn`.

## C14. Open Turn Beats Silence

A node with an open turn remains `working` even if the session file has been
silent longer than the debounce window. Long-running silent builds or network
tools must never be declared idle until a real close marker appears.
