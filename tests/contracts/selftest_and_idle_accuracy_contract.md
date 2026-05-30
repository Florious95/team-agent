# Selftest Comms and Idle Accuracy Contract

This contract covers `team-agent doctor --comms` and the bug-071 idle accuracy
rules. Implementers may read this document and the contract stubs only; the
acceptance tests are owned by the Module Contract Owner.

## Public Signatures

The public helper behind the CLI is:

```python
def run_comms_selftest(
    workspace: Path,
    *,
    team: str | None = None,
    gate: str | None = None,
    response_sla_sec: float = 20.0,
    probe_content: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]: ...
```

`driver` is an injectable boundary for tests. Production calls omit it and use
real tmux/runtime/message-store primitives. The helper returns stable JSON and
never writes probe text to the live leader pane.

The idle challenge helper is:

```python
def evaluate_idle_behavior(
    workspace: Path,
    *,
    agent_id: str,
    claimed_status: str,
    response_sla_sec: float = 20.0,
    token: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]: ...
```

## Feature A: doctor --comms

Final 0.2.8 scope: `doctor --comms` is a no-token, no-paste diagnostic. It
does not create throwaway sessions, does not send to live workers, does not ask
workers to report back, and does not claim live runtime end-to-end or code
correctness success. It performs one substantive gate plus honesty metadata:

1. `receiver_binding`: pure state read of live owner/receiver binding.
2. `contract_suite`: deferred to 0.2.9 because npm packages do not currently
   ship the required test files.

C1. `doctor --comms` extends the existing `team-agent doctor` surface. There is
no new top-level `team-agent selftest` command. `team-agent doctor --help`
documents `--comms`.

C2. `doctor --comms --json` and `doctor --gate comms --json` route to the same
helper. For the same workspace and same tick, their canonical JSON is byte
identical after timestamp/run-id fields are removed.

C-RT-8. The help first line and non-JSON banner must state the boundary:
`validates live pane binding consistency. Does NOT perform live runtime message
round-trip. comms contract suite deferred to 0.2.9 (test files not shipped).
(zero token, zero pollution)`.

C-RT-5. Top-level JSON has `scope="binding_consistency"` and must not use
`scope="code_correctness_and_binding"` or
`scope="live_link_runtime_end_to_end"`. Every non-deferred check has a
`verifies` value: `receiver_binding.verifies="binding_consistency"` and
`provider_sdk_calls.verifies="no_provider_sdk_calls"`.

C-RT-1. A `status=pass` check must include physical evidence for its category:
`receiver_binding` has `proof="state_read"` and `state_read_observed=true`.
Deferred checks are neutral and must not be treated as pass.

Receiver binding. The check reads live state only. It verifies that
`team_owner.pane_id`, `leader_receiver.pane_id`, and the current caller pane are
consistent when those fields exist, without command-name or UUID hard gates. It
does not call owner-population/first-bind mutators and does not persist the
disposable receiver.

Contract suite. For 0.2.8 this check must report
`status="deferred"`, `deferred_to="0.2.9"`,
`reason="contract test files not shipped with package"`, and a user-visible
message containing `comms contract verification deferred to 0.2.9`. It is
neutral: it does not make top-level `ok=false` when receiver binding passes and
provider SDK call counts are zero.

MUST-NOT-13. `doctor --comms` must not call provider SDKs or network clients
used for provider APIs. JSON reports call counts for `anthropic`, `openai`, and
`httpx`. Any count greater than zero fails the diagnostic.

Removed old mechanisms. `doctor --comms` must not expose or execute A1/A2/B1/B2
round-trip probes, throwaway tmux sessions, throwaway workspaces, stale selftest
cleanup, worker-to-leader pollution scans, or live message delivery. Those
behaviors belong to the standalone contract suite or future real-machine E2E,
not to the doctor command itself.

## Feature B: Idle Accuracy

C14. The latest provider idle prompt remains the strongest signal. Pane delta or
old working scrollback must not override a fresh idle prompt. This is backed by
verbatim real pane captures for both Codex
(`tests/fixtures/idle_prompts/codex_idle.txt`) and Claude Code
(`tests/fixtures/idle_prompts/claude_code_idle.txt`); the first metadata line is
not part of the scrollback input.

C15. Active task is required before pane-delta `running` can be promoted to
`WORKING`. Without an active task, raw `running` plus pane delta may remain
`IDLE`.

C16. Gap 32 takeover stays on provider session-log facts. The
`idle_takeover_wiring` module must not import `agent_health` or
`approvals.status`, and its inputs must not grow `last_output_at` or
`activity_output_hash`.

C17. Stuck detection includes `WORKING` rows as well as `RUNNING`. A worker with
an active task and stale `last_output_at` must still emit stuck diagnostics
after Feature B introduces the `WORKING` label.

## Real-Machine Acceptance

A realistic tester must run `team-agent doctor --comms --workspace . --json`
inside a live team and confirm:

1. No worker pane receives selftest text, no leader pane receives selftest text,
   and no new `ta-selftest-comms-*` tmux session or `/tmp` workspace is created.
2. JSON has `scope="binding_consistency"` and does not contain
   `code_correctness_and_binding` or `live_link_runtime_end_to_end`.
3. `receiver_binding` passes by state read and names the live bound pane.
4. `contract_suite` reports deferred to 0.2.9 because tests are not shipped.
5. Provider SDK call counts remain zero.
6. `team-agent status --json` still shows an active outputting worker as
   `WORKING` or `RUNNING`, not `IDLE`; after provider idle prompt returns, it
   shows `IDLE`.
