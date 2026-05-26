# Findings — spark-reviewer (0.2.2 slice-1, batch 2)

- **LOW** 45960c1 — `src/team_agent/coordinator/__main__.py:50`
  - Self-orphan exit is only triggered when `current_ppid == 1`, so launchd-supervised reparenting patterns (common on macOS where parent can be a shim process) can miss orphan coordinators. A coordinator can therefore keep running after test-workspace teardown when ppid is not literal `1`.
  - Suggested fix: broaden orphan detection to cover launchd shim parent paths and/or explicit parent/command drift (e.g. missing workspace + non-live parent) so termination is not tied to `ppid==1`.

- **MEDIUM** 45960c1 — `src/team_agent/events.py:52-79`
  - `_maybe_rotate()` performs a multi-step archive rename chain while swallowing every `OSError`; if any step partially succeeds and later steps fail, archive retention invariants can become inconsistent. In the worst case, rotation proceeds silently off-contract and forensic history/retention expectations are violated.
  - Suggested fix: make rotate a bounded two-phase operation (staging temp + single commit path), propagate/emit rotation failure when not fully committed, and avoid silent swallow of replace failures.

- **LOW** 945948b — `src/team_agent/message_store/result_watchers.py:114-125`
  - The legacy `claim_leader_notification()` API is now a hard `deprecated_noop`; any external caller still invoking it gets a no-op status and can proceed as if a newer/alternate dedupe layer is in effect, reintroducing duplicate-leader-notification risk without hard failure visibility.
  - Suggested fix: gate this shim behind an explicit runtime warning or hard failure path (with migration note + metrics/event) so accidental callers are surfaced immediately instead of silently disabling the old guarantee.

## Severity counts

- CRITICAL: 0
- HIGH: 0
- MEDIUM: 1
- LOW: 2
