# bug-084 / bug-085 0.2.11 Hotfix Contract

This handoff is the implementer-readable contract. The acceptance tests are authoritative; implementation should make `python3 -m unittest discover -v` pass without editing these tests or fixtures.

## bug-084 State Persistence Resilience

Scope is limited to `save_runtime_state`, `coordinator_tick`, `team_agent.coordinator.__main__.main`, and the already identified compaction save sites.

Required behavior:

- `save_runtime_state` keeps the normal atomic `tmp -> os.replace(tmp, state.json)` path.
- `PermissionError(EACCES)` and `OSError` with `errno` in `EACCES`, `EPERM`, or `EBUSY` are retried with bounded backoff. Other OS errors, such as `ENOSPC`, are not retried.
- After retry exhaustion, self-heal rebuilds the inode by writing a distinct heal temp file, renaming the original `state.json` to a backup, then replacing the state path with the heal temp. It must not use in-place truncate.
- If self-heal still fails, the original state remains valid and visible, and the failure is logged.
- Successful self-heal emits `runtime.state.self_healed`; retry attempts emit `runtime.state.save_retry`; final failure emits `runtime.state.save_failed`.
- `save_runtime_state` is serialized with the existing `_runtime_lock(workspace, "state-save", timeout=2.0)`.
- If the incoming state deep-equals `_RUNTIME_STATE_CACHE[str(path)]`, `save_runtime_state` returns before taking the lock or calling `os.replace`.
- `coordinator_tick` wraps only the tick-end save. A tick-end save failure returns a structured degraded envelope with `ok=False`, `reason="persistence_degraded"`, and `persisted=False`, and emits `runtime.state.save_failed` with `phase="tick_end"`.
- `team_agent.coordinator.__main__.main` catches `Exception` from `runtime.coordinator_tick`, emits deduped `coordinator.tick_error` / suppressed events, sleeps with exponential backoff `5, 10, 20, 40, 60, 60...`, and resets after a successful tick.
- No bug-084 path may call provider/network SDKs (`anthropic.messages.create`, `openai.chat.completions.create`, `httpx.post`).

Fixture: `tests/fixtures/bug_084_state_resilience/state-rich.json` is a non-empty runtime state with multiple agents, teams, tasks, and coordinator metadata.

## bug-085 compatible_api Claude Fallback and Idle Observability

Fixture provenance: the preferred local true compatible_api Claude worker fixture could not be captured because this machine has no visible compatible_api Claude profile/config that can be started without entering a real provider workflow. Per the CR verdict, the contract uses a constructed transcript fixture with an unparsable first line and a legal idle Claude assistant tail:

- `tests/fixtures/bug_085_compatible_api_claude/compatible_api_claude_idle_bad_first_line.jsonl`

Required behavior:

- For provider `claude` with `auth_mode="compatible_api"`, if strict `find_claude_transcript` cannot recover a `session_id`, the adapter scans the encoded cwd project directory by mtime and fills `rollout_path`.
- The fallback result sets `session_id=None`, `captured_via="fs_mtime_fallback"`, and `attribution_confidence="low"`.
- Native/subscription Claude paths do not use the fallback.
- Strict `find_claude_transcript` still wins when it can produce a true `session_id`.
- The half-state `session_id=None` and `rollout_path=set` is legal: idle classification uses `rollout_path`, resume refuses cleanly with `ResumeUnavailable`, restart/reset/status/idle consumers do not crash.
- `build_idle_nodes` classifies the fixture tail as idle, not unknown.
- Native Claude Code (`provider="claude_code"`) and compatible-api Claude (`provider="claude"`, `auth_mode="compatible_api"`) both use the same Claude transcript turn-state reader for idle/take-over. A real Claude Code transcript with `message.stop_reason="end_turn"` and metadata tail must classify as idle when `rollout_path` is present.
- If `rollout_path` is still missing, the node remains unknown and must not count as idle. This preserves the bug-071/bug-077 false-IDLE boundary; the fix must remove provider/capture-path divergence, not treat unknown as idle.
- `evaluate_takeover_reminder` emits `idle_takeover.no_ping` only when the no-ping reason changes.
- Long-term unknown nodes do not count as idle and do not ping. Starting at 60 consecutive ticks, the coordinator emits `idle_takeover.unknown_persistent` every 12 ticks with `node_id`, `provider`, `auth_mode`, `consecutive_ticks`, and `rollout_path`.
- The fallback helper must not call strict `find_claude_transcript`.
- No bug-085 fixture path may call provider/network SDKs (`anthropic.messages.create`, `openai.chat.completions.create`, `httpx.post`).
