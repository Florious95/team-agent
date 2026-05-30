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
workers to report back, and does not claim live runtime end-to-end success. It
performs exactly two substantive checks plus honesty metadata:

1. `receiver_binding`: pure state read of live owner/receiver binding.
2. `contract_suite`: run an allowlisted communications contract suite against
   the same installed Team Agent code used by the live team.

C1. `doctor --comms` extends the existing `team-agent doctor` surface. There is
no new top-level `team-agent selftest` command. `team-agent doctor --help`
documents `--comms`.

C2. `doctor --comms --json` and `doctor --gate comms --json` route to the same
helper. For the same workspace and same tick, their canonical JSON is byte
identical after timestamp/run-id fields are removed.

C-RT-8. The help first line and non-JSON banner must state the boundary:
`validates comms code correctness (contract suite on installed code) + live
pane bindings. Does NOT perform live runtime message round-trip. (zero token,
zero pollution)`.

C-RT-5. Top-level JSON has `scope="code_correctness_and_binding"` and must not
use `scope="live_link_runtime_end_to_end"`. Every check has a `verifies` value:
`receiver_binding.verifies="binding_consistency"`,
`contract_suite.verifies="code_correctness"`, and
`provider_sdk_calls.verifies="no_provider_sdk_calls"`.

C-RT-1. A `status=pass` check must include physical evidence for its category:
`receiver_binding` has `proof="state_read"` and `state_read_observed=true`;
`contract_suite` has `pytest_executed=true`, `pytest.exit_code=0`, non-empty
`pytest.tests_run`, and a non-empty `pytest.counts` dict. Missing evidence is a
failure, not a default pass.

Receiver binding. The check reads live state only. It verifies that
`team_owner.pane_id`, `leader_receiver.pane_id`, and the current caller pane are
consistent when those fields exist, without command-name or UUID hard gates. It
does not call owner-population/first-bind mutators and does not persist the
disposable receiver.

Contract suite. The allowlist is explicit and stable enough to audit. It must
include the representative communications tests:
`tests.test_messaging_tmux`, `tests.test_send_busy_recipient_acceptance`,
`tests.test_messaging_leader_receiver_buffer`,
`tests.test_selftest_and_idle_accuracy_acceptance`,
`tests.test_messaging_leader`, `tests.test_messaging_mcp`,
`tests.test_worker_peer_delivery_scheduling`, and
`tests.test_result_delivery_contract`. The result contains
`pytest.{exit_code, tests_run, counts, duration_seconds, warnings}`.

C-RT-6. The suite runs against the same installed Python and Team Agent package
as the live team. JSON includes
`pytest_env.{python_path, team_agent_version, site_packages_path}` and a live
environment snapshot. Any mismatch fails with `error="install_mismatch"` and a
list of mismatched fields.

C-RT-7. `tests_run` must be non-empty. `tests_run=[]` fails with
`reason="no_tests_run"`. `skipped>0 and passed==0` fails with
`reason="all_relevant_tests_skipped"`. The exit code and counts are mandatory.

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
2. JSON has `scope="code_correctness_and_binding"` and does not contain
   `live_link_runtime_end_to_end`.
3. `receiver_binding` passes by state read and names the live bound pane.
4. `contract_suite` runs the installed-code allowlist and reports non-empty
   `tests_run`, counts, exit code, duration, environment, and package path.
5. Provider SDK call counts remain zero.
6. `team-agent status --json` still shows an active outputting worker as
   `WORKING` or `RUNNING`, not `IDLE`; after provider idle prompt returns, it
   shows `IDLE`.
