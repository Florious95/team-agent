# Findings — spark-reviewer (Slice 2 follow-up)

## Scope
- `617a517` `src/team_agent/messaging/tmux_io.py`, `src/team_agent/messaging/leader.py`, `src/team_agent/messaging/trust_auto_answer.py`, `src/team_agent/messaging/leader_panes.py`, `tests/test_gap29_send_trust_prompt_integration.py`
- `6b1fc75` `src/team_agent/restart/orchestration.py`, `tests/test_runtime_core_07.py`, `tests/test_runtime_core_10.py`

## 2026-05-26 Review — 617a517 (`gap-29` canonical non-input gate)

### [MEDIUM] Leader-receiver trust-prompt branch lacks retry scheduling parity

Commit: `617a517`
File/line evidence: `src/team_agent/messaging/leader.py:254-266`, `src/team_agent/messaging/trust_auto_answer.py:21-37`, `src/team_agent/messaging/delivery.py:173-193`
Description: In the leader-receiver lane, a trust prompt that is still present after waiting (`_wait_for_trust_prompt_dismissal` timeout) is converted to a terminal failure envelope and immediately handed to `_fail_leader_delivery`. Unlike worker delivery (`_deliver_pending_message`), this path does not emit `trust_retry_needed`/`trust_retry_scheduled` events or enqueue `trust_retry`, so transient slow prompt-dismiss races become one-shot drops to fallback instead of bounded retries.
Suggested fix shape: on timeout in `retry_injection_after_trust_auto_answer`, mirror the worker behavior by scheduling a retry event (or reusing `_handle_trust_retry_needed`) with a bounded backoff and explicit terminal exhaustion event for repeated failures, then keep existing success/error semantics unchanged.

### [LOW] Leader receiver integration test does not cover post-detection timeout failure path

Commit: `617a517`
File/line evidence: `tests/test_gap29_send_trust_prompt_integration.py:136-237`
Description: The new leader-receiver integration scenario only exercises the happy path where the prompt disappears. There is no test for the `trust_prompt_not_dismissed_after_answer` branch, so regressions in `retry_injection_after_trust_auto_answer` timeout behavior or loss of diagnostics are currently uncaught.
Suggested fix shape: add a failure-path test that keeps capture in trust-prompt state longer than the dismissal timeout and asserts the resulting event status/queue behavior (retry vs terminal fallback) before/after applying the above fix.

## 2026-05-26 Review — 25b2249 (`gap-29` trust prompt detection above trailing blank scrollback)

### [LOW] Detection can still miss prompts buried beyond the fixed 30-line window

Commit: `25b2249`
File/line evidence: `src/team_agent/messaging/tmux_io.py:303-304`, `src/team_agent/messaging/tmux_io.py:370-374`, `src/team_agent/messaging/tmux_prompt.py:45-53`
Description: Pre-paste detection still relies on a fixed capture of 30 tmux lines and a 15 non-empty-line normalization window, so prompts separated from the tail by heavy output (e.g. compile spam or bursty progress logs) can remain undetected and proceed without refusal handling.
Suggested fix shape: add bounded fallback when no mode/prompt is detected—e.g. widen capture adaptively (e.g. full visible history) or trigger a secondary capture/retry when the first window is mostly non-prompt output.

### [LOW] ANSI strip only removes CSI sequences, leaving OSC/control escapes in the detection path

Commit: `25b2249`
File/line evidence: `src/team_agent/messaging/tmux_prompt.py:16`, `src/team_agent/messaging/tmux_prompt.py:50`
Description: `_ANSI_ESCAPE_RE` removes only `ESC[` CSI escapes; OSC sequences (`ESC]...BEL/ST`) and other terminal control payloads are not stripped and can fragment signature text in real Codex output before regex matching.
Suggested fix shape: switch to a terminal-control normalizer that strips OSC as well (or use a broad ANSI cleaner) before non-input detection.

## 2026-05-26 Review — a3d4fe5 (`gap-29` startup-prompt wait & stale non-input suppression)

### [MEDIUM] Input-ready marker can be over-eager on non-input lines and mask real prompts

Commit: `a3d4fe5`
File/line evidence: `src/team_agent/messaging/tmux_prompt.py:93-99`, `src/team_agent/messaging/tmux_prompt.py:75-90`
Description: `_is_input_ready_prompt` treats any line ending in `codex|claude` + `> / › / ❯` as ready. In mixed scrollback where a stale non-input line contains that tokenized pattern (e.g., status/caption text that includes `codex >`), `_stale_non_input_before_ready_prompt` can mark `latest_ready` after `latest_non_input` and suppress trust/pause prompt detection. That can leave active prompts unhandled while returning `None` (input-ready) downstream.
Suggested fix shape: tighten ready-pattern matching to shell-like prompt contexts only (for example, prompt-marker anchored as start-of-line with optional whitespace and minimal CLI prompt grammar) and guard codex/claude suffix matching with stronger delimiters or capture state.

### [LOW] Startup prompt wait-parameter sweep is not fully wired/observable

Commit: `a3d4fe5`
File/line evidence: `src/team_agent/approvals/runtime_prompts.py:67`, `src/team_agent/launch/core.py:218`, `src/team_agent/runtime.py:668-678`
Description: The commit updates `handle_startup_prompts` calls to `checks=20, sleep_s=0.5` in three direct callsites, but `_handle_startup_prompts_and_verify_window` in `runtime.py` is now an isolated wrapper with the same params and no in-tree callsite. This creates an unclear/unused startup path and makes “5 callsite” coverage from the brief hard to verify.
Suggested fix shape: either call the shared helper from all intended startup paths (and assert via tests that no call path uses defaults), or delete the unused wrapper and make the effective callsite set explicit.

### [LOW] Empty/history-less first-screen acceptance scenario is still untested

Commit: `a3d4fe5`
File/line evidence: `tests/test_pane_state_classifier_acceptance.py:11-29`, `tests/test_pane_state_classifier_acceptance.py:31-39`
Description: Existing acceptance tests do not include a zero-history/no-scrollback capture (e.g., empty capture + first visible prompt-only `>`/`›`), so behavior changes in `_is_input_ready_prompt`/`_stale_non_input_before_ready_prompt` cannot be regression-guarded for the exact “fresh tmux first-screen” edge the brief called out.
Suggested fix shape: add one direct fixture test for `detect_non_input_scrollback("")` and one for `" > "`-style first-screen tails so future regex tuning must preserve the intended `input_ready` result.

## 2026-05-26 Review — f3d00f7 (`gap-29` spec deprecation schema gate fix)

### [LOW] Runtime schema-validator drift still exists beyond this committed fix

Commit: `f3d00f7`
File/line evidence: `schemas/team.schema.json:60-83`, `src/team_agent/spec.py:185-199`, `src/team_agent/spec.py:209-211`
Description: This commit correctly adds `auto_trust_own_workspace` to both schema and validator allowed set, but the runtime block still has drift elsewhere: validator still accepts `auto_attach_leader`, `fast`, `tick_interval_sec`, `push_min_interval_sec`, and `stuck_timeout_sec` while schema `additionalProperties: false` excludes them. Tooling that relies on schema validation (IDE/schema-driven docs, CI lint) will still reject these valid keys, so schema remains non-authoritative and can block legitimate specs.
Suggested fix shape: align schema and runtime validator by either adding those runtime fields with matching types/doc metadata to schema or tightening validator to reject them if deprecation policy changed; then add a drift test that asserts a single symmetric key set for `/runtime`.

## 2026-05-26 Review — 6b1fc75 (`restart` atomicity pre-check)

### [MEDIUM] Pre-check exclusion context diverges from restart-loop repair context

Commit: `6b1fc75`
File/line evidence: `src/team_agent/restart/orchestration.py:345-384`, `src/team_agent/restart/orchestration.py:150-164`, `src/team_agent/sessions/resume.py:56-60`
Description: `_atomic_resumability_check` uses `known_session_ids` derived only from persisted state, while the later `_prepare_resume_state` call uses `state + new_agents`. A worker can pass pre-check with a repaired session candidate and still fail during restart when an earlier repaired session is excluded later, so the operation escapes atomic refusal and fails mid-loop after teardown has begun.
Suggested fix shape: share a single candidate-resolution helper between pre-check and restart loop (including progressive exclusion of repaired sessions), and treat repair failures there exactly as restart-loop `ResumeUnavailable` outcomes.

### [LOW] New `restart.atomic_refusal` event has no contract assertions

Commit: `6b1fc75`
File/line evidence: `src/team_agent/restart/orchestration.py:103-118`
Description: `restart.atomic_refusal` is a new observability event for the refusal path but currently has no dedicated regression assertions. Its payload stability (`unresumable` structure, `reason`, `allow_fresh`) is therefore unverified and can drift without test protection.
Suggested fix shape: add a focused test that forces refusal and asserts emitted event payload keys/shape for deterministic downstream consumption before this becomes a hard dependency.

## 2026-05-27 Review — ff99026 (`session capture retry + loud-fail semantics`)

### [LOW] Attention event is emitted continuously for snapshot checks with `timeout_s=0`

Commit: `ff99026`
File/line evidence: `src/team_agent/sessions/capture.py:39-54`, `src/team_agent/sessions/capture.py:108-126`, `src/team_agent/coordinator/lifecycle.py:278`, `src/team_agent/status/queries.py:27`, `src/team_agent/messaging/send.py:249,308`
Description: `capture_missing_sessions` calls `capture_agent_session` with `timeout_s=0.0` from high-frequency paths (status/coordinator/send/collect) and `log_miss=False`, but `capture_agent_session` still writes `session.capture_required_attention` on every timeout when `status == "running"` regardless of whether this miss is retry-budgeted by the caller. A transiently missing session_id on a single call can therefore emit a new attention event on each poll cycle, causing log churn and noisy attention telemetry for persistent misses.
Suggested fix shape: add a bounded/once-per-agent debounce for `session.capture_required_attention` (or a monotonic retry state in memory/state) so repeated zero-timeout probes do not emit duplicate alerts each call.

### [LOW] Loud-fail contract (`raise_on_missed=True`) is currently unreachable from production paths

Commit: `ff99026`
File/line evidence: `src/team_agent/lifecycle/start.py:322`, `src/team_agent/lifecycle/operations.py:355`, `src/team_agent/restart/orchestration.py:301`, `src/team_agent/launch/core.py:258`, `src/team_agent/sessions/capture.py:72`
Description: The new default strict path was introduced on `capture_agent_session`, but all production callsites now pass `raise_on_missed=False` (plus `capture_missing_sessions` has it hard-coded). No non-test caller in the repo invokes the default path today, so the loud-fail contract can be exercised only via direct import/tests and does not actually guard normal spawn/attach flows.
Suggested fix shape: either document this as an explicit internal best-effort policy and rename the default to `raise_on_missed: bool = False`, or provide a dedicated production caller (if any) that intentionally owns the atomic boundary for strict missing-session failure handling.

## 2026-05-27 Review — 0894529 (`spec` load-time deprecation emission)

### [LOW] `load_spec` writes spec deprecation events to `path.parent/.team/logs` for any caller path, not verified workspace root

Commit: `0894529`
File/line evidence: `src/team_agent/spec.py:30`, `src/team_agent/spec.py:41-43`, `src/team_agent/spec.py:53`, `src/team_agent/diagnose/health.py:27`, `src/team_agent/messaging/scheduler.py:153`
Description: `_emit_load_time_deprecations` assumes `EventLog(path.parent)` is the correct workspace audit root for every `load_spec` caller. In practice, `load_spec` is reused in helper paths (diagnostics/test fixtures/spec reads) where `path` can be non-workspace inputs; this can create/append `.team/logs/events.jsonl` beside an arbitrary spec location and makes deprecation logging context-dependent on caller semantics rather than runtime root.
Suggested fix shape: accept an explicit audit/log workspace argument (or derive from an established `workspace_root`) and only emit load-time deprecation events when that workspace context is known/validated.

### [MEDIUM] Runtime trust-prompt path can emit a second `trust_auto_answer_spec_opt_in_deprecated` event for the same opt-in decision

Commit: `0894529`
File/line evidence: `src/team_agent/spec.py:30`, `src/team_agent/messaging/leader_panes.py:544-546`, `src/team_agent/messaging/leader_panes.py:605-616`, `tests/contracts/spec_deprecation_contract.md:3`
Description: With this commit, deprecated-field detection now emits once at `load_spec` and again whenever `attempt_trust_auto_answer` calls `_auto_trust_opt_in` (if the same spec is still in play during prompt handling). That path already writes `trust_auto_answer_spec_opt_in_deprecated` per call, so a real workflow can generate two audit events for one startup/run and blur load-time vs runtime-trigger semantics.
Suggested fix shape: make the event source explicit (`trigger="load_spec"` vs `trigger="prompt_path"`) and/or suppress the runtime event when load-time emission has already occurred for the same process/workspace/message lifecycle.

### [LOW] Deletion guard is per-process only, so multi-process launches can still spam stderr warning

Commit: `0894529`
File/line evidence: `src/team_agent/messaging/leader_panes.py:622-633`, `src/team_agent/messaging/leader_panes.py:645`, `src/team_agent/messaging/leader_panes.py:648-653`
Description: `_SPEC_OPT_IN_DEPRECATION_WARNED` is a module-global flag; any subprocess/forked worker emits the deprecation warning independently. In multi-process test/prod paths this can surface repeated warnings even when each process loads the same spec in the same workspace session.
Suggested fix shape: if the product needs cross-process suppression, persist a short-lived runtime marker under `.team/logs` and gate warning writes per workspace session, while keeping events per spec load untouched.

## 2026-05-27 Review — 9dfedae (`spec` workspace_root resolution for deprecation logs)

### [LOW] Workspace-root resolver is path-literal and not symlink-canonical, so equivalent spec inputs can split audit logs

Commit: `9dfedae`
File/line evidence: `src/team_agent/spec.py:60-81`
Description: `_resolve_workspace_root` walks the un-resolved `spec_path` string, so `load_spec` on a symlinked spec file (`alias/team.spec.yaml`) resolves to `alias` rather than the underlying workspace root that actually owns `.team`. That can send `trust_auto_answer_spec_opt_in_deprecated` into the symlink container and split audit streams for one logical workspace when run with mixed spec paths.
Suggested fix shape: canonicalize `spec_path` first (e.g., `spec_path = spec_path.expanduser().resolve(strict=False)`) before ascending ancestors, or explicitly document this as intentional "path alias not supported" behavior in the contract.

## 2026-05-27 Review — b0cf773 (`restart` Route B first_send_at gates)

### [LOW] Leader identity fallback can suppress first_send_at stamps after migration or rename

Commit: `b0cf773`
File/line evidence: `src/team_agent/messaging/delivery.py:351-353`
Description: `leader_id` is resolved from `state["leader"]["id"]` with a fallback of `"leader"`, and only `{"leader", "Leader", leader_id}` is treated as the leader sender. If the state is missing `leader` during migration/repair (or leader id is customized), leader→worker deliveries can be un-stamped and those workers are then considered never-interacted on restart.
Suggested fix shape: derive a deterministic sender identity source (e.g., explicit leader identity from runtime metadata and sender-not-in-agents fallback) and avoid silently skipping first_send_at when leader metadata is absent.

### [MEDIUM] `first_send_at` classification uses loose truthiness and is sensitive to malformed state

Commit: `b0cf773`
File/line evidence: `src/team_agent/restart/orchestration.py:398-406`, `tests/contracts/restart_resume_atomicity_contract.md:11-12`
Description: `_atomic_resumability_check` uses `if not first_send_at`, so invalid values (`""`, `0`, `false`) are treated as never-interacted, while garbage strings like `"null"` are treated as interacted. This can misclassify restart behavior under legacy/corrupt state mutations and cause either false atomic refusal or unintended fresh-start.
Suggested fix shape: replace truthiness checks with explicit contract checks (`None`/missing => not interacted; strict ISO-8601 parse => interacted; invalid type/value => emit repair event + deterministic fallback) and keep behavior stable for restart decisions.

## 2026-05-27 Review — b695843 (`Route B strict-typing + auditability`)

### [LOW] `_classify_first_send_at` accepts naive ISO-8601 strings as valid interacted timestamps

Commit: `b695843`
File/line evidence: `src/team_agent/restart/orchestration.py:381-403`
Description: `_classify_first_send_at` accepts any non-empty `datetime.fromisoformat` value as valid, including naive local timestamps like `2026-05-27T10:23:00` with no UTC marker. That can classify a worker as interacted even when the persisted value is not explicitly UTC, while the contract/error text and restart docs say UTC timestamp semantics.
Suggested fix shape: enforce UTC awareness (`tzinfo is not None` and offset=UTC) before returning `valid`; reject naive timestamps as `corrupt` to keep decision logic deterministic across nodes/clock contexts.

### [LOW] Atomic refusal detail text always says session is missing

Commit: `b695843`
File/line evidence: `src/team_agent/restart/orchestration.py:496-517`
Description: `_format_atomic_refusal_error` always emits `"its persisted session is missing"` for refused workers, but refusal reasons can be `session_unresumable` as well as `no_persisted_session_id`. The human message can therefore mislead operations when a session exists but cannot be recovered.
Suggested fix shape: format refusal evidence by reason (`missing` vs `unresumable`) and include the relevant session id/repair rationale in each worker clause.

### [LOW] Resume-decision events are emitted at full fanout every restart call

Commit: `b695843`
File/line evidence: `src/team_agent/restart/orchestration.py:127-133`, `src/team_agent/restart/orchestration.py:483-492`
Description: `restart()` now emits `restart.resume_decision` for every non-paused worker on each restart invocation, including repeated automation retries. If restart is scripted in tight loops, event volume can scale with worker count × retries and increase log churn, whereas the previous path only surfaced refusals.
Suggested fix shape: keep decision events for explicit restart CLI invocations, and add either batching/level gating or periodic coalescing for automation-heavy call paths.
