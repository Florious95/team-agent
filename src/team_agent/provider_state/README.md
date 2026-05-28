# Adding a provider idle/turn-state adapter

Gap 32 decides every node's idle/working/abnormal state from a deterministic
FILE FACT — the provider's own session-log/rollout turn-lifecycle records — never
from the pane screen. The predicate, abnormal track, and wake layers are
**provider-neutral and reused unchanged**. To support a brand-new CLI you fill the
small checklist below; you do not touch any neutral module.

## What you add (only two places)

1. `src/team_agent/provider_state/<provider>.py` — a thin reader that translates
   that CLI's session records into normalized lifecycle facts.
2. one entry in `src/team_agent/provider_state/registry.py` — pure infra DATA.

Everything else (`idle_predicate.py`, `abnormal_track.py`, `wake.py`,
`idle_takeover.py`) is provider-neutral and must stay free of provider names
(there is a grep test, C6).

## The checklist

### 1. Session/rollout file location
- Where does this CLI write its per-session log? (root dir + path layout)
- How does the framework already learn each agent's path? (it is captured into
  runtime state per agent as `rollout_path`; confirm yours lands there.)
- Record it under the registry entry `file_location`.

### 2. Turn-lifecycle event types (do the empirical capture FIRST)
Capture REAL records from a live session for each state and record the exact
record `type`/field. These become the contract fixtures (real-fixture-first):
- **turn-started / open turn** — the marker that a turn is in flight.
- **turn-complete** — the close that means idle.
- **interrupted** — user ESC / abort (idle_interrupted, idle + red note).
- **blocked / approval** — awaiting a human decision (blocked_on_human).
- **error / failed** — a structured terminal fault record.
Implement these as `extract_facts(records) -> (facts, diagnostics)` in your reader,
emitting `team_agent.provider_state.common` fact kinds: `TURN_OPEN`,
`TURN_COMPLETE`, `INTERRUPTED`, `FAILED`, `APPROVAL`, `ERROR`. Fault facts should
carry `signature`, `turn_id`, and `raw` (the original record). Filter out trailing
metadata/telemetry records so the verdict is the last LIFECYCLE fact, not the last
physical line.

Reference markers already implemented:
- Claude transcript: assistant `stop_reason==end_turn` (idle) / `==tool_use`
  (working); user text `[Request interrupted by user]` (interrupted); user
  `tool_result is_error==true` and system `subtype==api_error,level==error` (faults).
- Codex rollout: `event_msg payload.type==task_started|task_complete`;
  `turn_aborted reason==interrupted`; app-server `turn.status==failed` and
  `*/requestApproval`.

### 3. Black/white list seed entries
- `error_lists.whitelist` — record/string patterns that are benign → skip.
- `error_lists.blacklist` — known error signatures → notify (`api error`,
  `rate limit`, `overloaded`, traceback/panic, provider `failed`, ...).
- Precedence is whitelist > blacklist > default-notify (catch-bias for structured
  faults only). Lists are DATA — adding a pattern is one edit + one fixture.

### 4. Optional hook accelerator
- Does the CLI expose hooks that fire on turn boundaries (e.g. a `Stop`/`Notify`
  program)? If so they can push a fact row to wake the watcher faster — but the
  file fact remains the source of truth (the hook is validated against the file,
  never the sole signal).

### 5. Process/identity facts for the liveness guard
- How to read the provider process identity (start-time / cmdline) so an open
  turn whose process was replaced (PID reuse) classifies as `crashed_mid_turn`,
  never eternal `working` (C4). `provider_state.common.process_is_live` already
  implements the comparison given `{"expected": {...}, "current": {...}}`.

## Reused unchanged (do NOT modify per provider)
- `idle_predicate.evaluate_takeover_reminder` — all-idle + arm-after-delegation +
  monotonic debounce + edge ack.
- `abnormal_track.process_abnormal_records` / `detect_whole_team_gone` — dedup,
  catch-bias, coordinator-independent whole-team-gone.
- `wake` — file-change watch + mtime gate.
- `idle_takeover` — the public facade.

If you find yourself editing a neutral module to add a provider, stop — the fact
you need belongs in the reader or the registry entry instead.
