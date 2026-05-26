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
