# Findings — spark-reviewer (Slice 2 supplement)

## Scope
- `f89ba47` `tests/test_gap38_repro_four_deliveries.py`
- `65d426f` `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`
- `3fd1f1d` `src/team_agent/messaging/leader_panes.py`, `src/team_agent/messaging/delivery.py`, `tests/test_gap29_trust_auto_answer.py`
- `e7892cf` `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`, `tests/test_gap38_repro_four_deliveries.py`
- `10078e7` `src/team_agent/messaging/delivery.py`, `src/team_agent/messaging/leader_api_errors.py`, `src/team_agent/messaging/leader_panes.py`, `tests/test_gap29_trust_auto_answer.py`, `tests/test_gap28_event_emission.py`

> Items 1-3 were swept by `e7892cf` (clock advancement in scheduler-retry harness;
> per-thread deepcopy in parallel-threads harness; API-context prefix on the
> leader_api_errors patterns). Items 4-7 were swept by the spark-sweep-2 bundle
> commit (bounded poll for trust-prompt dismissal; canonical-path workspace
> match; structured events on every refusal branch; multi-line sliding-window
> API context matching).

## Findings

1. [MEDIUM] `10078e7` — `retry_needed` branch marks message as failed, so it cannot be retried
   - **File/line:** `src/team_agent/messaging/delivery.py:100`
   - **Issue:** `_wait_for_trust_prompt_dismissal` failure returns `status: "retry_needed"` and emits `trust_auto_answer_retry_needed`, but the same branch calls `store.mark(message_id, "failed", ...)`. `_deliver_pending_messages` only reprocesses `pending`/`accepted` messages, so this branch is now terminal in practice and can never be auto-retried by coordinator flow.
   - **Fix shape:** Either keep the message in a re-queueable state (`defer_delivery`/custom retry state + scheduled due event) or implement the coordinator/sender consumer that explicitly re-drives `retry_needed` into another delivery attempt before marking failed.

2. [MEDIUM] `10078e7` — API context matcher drops potential hits when a scan window exceeds char cap
   - **File/line:** `src/team_agent/messaging/leader_api_errors.py:159-161`
   - **Issue:** Any 1–3 line window with `len(window) > 400` is skipped wholesale, so verbose error lines can suppress all matches even when the final line contains a real API keyword/context pair. This regresses recall for long wrapped diagnostics.
   - **Fix shape:** Keep the window cap bounded without discarding content, e.g. scan each line (or rolling char-slice windows) with truncation per match segment, and/or cap only the amount searched per line while still checking a bounded context around suspected API/context tokens.
