# Findings — spark-reviewer (0.2.2 slice-1)

- **MEDIUM** 634f5cc — `src/team_agent/messaging/leader_panes.py:404`
  - In `_ambiguous_debounce_bucket()`, `now.second` is bucketed by `% _AMBIGUOUS_DEBOUNCE_SECONDS` (`60`) and then reinserted into the same minute/hour, so ambiguity notifications dedupe only by second-of-minute, not by elapsed 60-second window.
  - Suggested fix: bucket on epoch time (e.g. `int(now.timestamp() // _AMBIGUOUS_DEBOUNCE_SECONDS) * _AMBIGUOUS_DEBOUNCE_SECONDS`) and format from that value so each 60s window gets one incident id.

- **MEDIUM** da436a3 — `src/team_agent/rust_core.py:239`
  - `_parse_ps_eww_output()` falls back to `lines[1]` when the PID row is not found, which can parse env vars from an unrelated process (or fail-open), causing `leader_session_uuid`/identity to be read from the wrong pane during rediscovery and candidate matching.
  - Suggested fix: return `{}` on miss (or raise/skip that target) instead of using an arbitrary row, and only accept rows where `target_row` is the matching PID row.

- **MEDIUM** d9f740d — `src/team_agent/messaging/idle_alerts.py:125`
  - In `_scan_event_progress_signals()`, events without `team`/`owner_team_id` are treated as applicable to every team; in multi-team workspaces, activity in one team can suppress idle-fallback for another via shared event log.
  - Suggested fix: include owner-team in newly emitted progress events (`send.deliver_attempt`, `leader_receiver.deliver_attempt`, MCP progress events) or ignore unscoped events unless workspace has exactly one team in scope.

## Severity counts
- MEDIUM: 3
- HIGH: 0
- LOW: 0
