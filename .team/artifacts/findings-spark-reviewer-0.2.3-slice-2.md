# Findings — spark-reviewer (Slice 2 supplement)

## Scope
- `f89ba47` `tests/test_gap38_repro_four_deliveries.py`
- `65d426f` `src/team_agent/messaging/leader_api_errors.py`, `tests/test_gap28_event_emission.py`

1. [MEDIUM] `f89ba47` — test does not actually exercise scheduled retries
   - **File/line:** `tests/test_gap38_repro_four_deliveries.py:335-340`
   - **Issue:** The scheduler-retry reproduction loops `_fire_due_scheduled_events` up to 4 times without advancing time or stubbing now, but retries are scheduled at future `due_at` (`now + min(2*attempt, 5)`) in `src/team_agent/messaging/scheduler.py:124`. In normal operation, only the initial row is due on each loop, so the assertion that only one inject occurs cannot validate retry-dedupe behavior.
   - **Fix shape:** In this test, freeze/advance clock (`freezegun`/`monkeypatch datetime.now`) or explicitly rewrite retry row `due_at` before each cycle, and assert that retry rows transition from `retry_scheduled` → `done` while still deduping injection attempts.

2. [MEDIUM] `f89ba47` — state sharing makes concurrency case brittle and under-constrained
   - **File/line:** `tests/test_gap38_repro_four_deliveries.py:278-283` and `src/team_agent/messaging/leader.py:167`
   - **Issue:** `test_parallel_threads_same_result_id_dedupe_to_one_inject` shares a mutable `state` dict across threads while `_send_to_leader_receiver` mutates `state['leader_receiver']` during rediscovery/validation. A non-deterministic dict write interleaving can hide concurrency regressions or flake depending on scheduling, so the test does not reliably represent production duplicate-path behavior.
   - **Fix shape:** Pass a per-thread copied state (`copy.deepcopy`) into each worker or guard shared-state mutation inside the production path (or patch immutable snapshot), and assert no shared-state race by validating serialized state changes separately.

3. [MEDIUM] `65d426f` — API-error regex is too broad and can false-fire on legitimate scrollback text
   - **File/line:** `src/team_agent/messaging/leader_api_errors.py:39-45`
   - **Issue:** `_ERROR_PATTERNS` matches generic tokens such as `5xx`, `fetch failed`, and timeout words outside explicit API-context markers. In normal prompt/assistant output this can emit `leader.api_error` for non-API noise (for example, user text containing "503" or "fetch failed"), causing noisy/fake incidents.
   - **Fix shape:** Tighten patterns to require provider/API-error context (e.g., `API Error`, `HTTPError`, `request failed`, `codex/claude` prefixes), and add negative tests ensuring benign lines containing those keywords do not emit events.

4. [MEDIUM] `3fd1f1d` — fixed 0.3s delay may still race trust prompt dismissal
   - **File/line:** `src/team_agent/messaging/delivery.py:82-91`
   - **Issue:** After `attempt_trust_auto_answer` succeeds, delivery retries paste with a fixed `sleep(0.3)` and does not verify that codex trust prompt actually exited (`pane` back to input mode) before re-injecting. On slower terminals, the second paste can still hit the same `codex_trust_prompt` state and fail, then immediately return failure without surfacing a second structured branch in delivery.
   - **Fix shape:** Poll tmux state before retry (e.g., `_prepare_tmux_pane_for_input(target)` or prompt-capture signature) and only retry once prompt clears; if still non-input, return a dedicated retry-needed outcome or reschedule instead of hard-failing.

5. [MEDIUM] `3fd1f1d` — workspace-boundary check is a weak substring match
   - **File/line:** `src/team_agent/messaging/leader_panes.py:580-588`
   - **Issue:** `_capture_tail_references_workspace` accepts any tail containing the workspace string (`resolved in tail or raw in tail`), so prefixes can match unrelated paths (e.g. `/path/repo` in `/path/repo-backup`) and can falsely auto-answer trust prompts for wrong directories; symlink/`~`/trailing-slash variants can also false-negative and block intended auto-answer.
   - **Fix shape:** Normalize both prompt path and workspace path (resolve, realpath, `Path(...).as_posix()`), parse candidate dir token from prompt text, and compare normalized canonical paths with boundary-safe equality.

6. [LOW] `3fd1f1d` — partial opt-in refusal states are silent in events
   - **File/line:** `src/team_agent/messaging/leader_panes.py:539-550` and `src/team_agent/messaging/delivery.py:73-81`
   - **Issue:** `not_opted_in` and `pane_id_missing` refusals return structured reasons but only the generic `send.failed` event is emitted later, with no `leader_panes.trust_auto_answer_*` event for those branches. This makes the decision matrix less observable than the explicit `..._refused` path and can slow diagnosis of why trust auto-answer was skipped.
   - **Fix shape:** Emit structured events for all non-success branches (including `not_opted_in` and `pane_id_missing`) and include those reasons in the final send failure event payload.

7. [MEDIUM] `e7892cf` — API-context gating can miss real errors when logs wrap across lines
   - **File/line:** `src/team_agent/messaging/leader_api_errors.py:55-65`
   - **Issue:** New `_API_CONTEXT` coupling is constrained to a single logical line (`[^
]{0,120}`), so real provider/API diagnostics split by tmux line wraps (e.g., `claude:` then wrapped `request timed out` on next line) will now not match. That regresses recall in `leader.api_error` while reducing false positives.
   - **Fix shape:** Match context across nearby lines with a small sliding-window scan (or relax to allow newline between marker and keyword), and keep an upper bound to avoid overbroad matching.
