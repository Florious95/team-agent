# RB12 F16 R2 — macOS terminal-status timeout

Status: `unjudgeable; F17 remains blocked`.

## Scope freeze

- PR #95 branch: `portfolio/f16-wleak-terminal-routing`.
- Required starting and reproduction head: `86b3aec1065a1e48ce6d647d60e2a10f21be9182`; tree `35f12d48db7a0df3e848393cf33a63363b925fe9`.
- Accepted F14 `312d19a38330cf9389fa69f915709f6ec7477b1e` is an ancestor. No F17, release, candidate, workflow dispatch, rebase, or force operation was performed.
- Direct source scope remains the owned WLEAK test file.

## Authoritative red and bounded local result

- GitHub run `33232893985`, macOS job `99048692074`, failed the owned selector at `framework.rs:983`: `timed out after 6s waiting for: message reaches terminal status` for `msg_380ff2ce64a8`. This remains product/test-contract red.
- The clean task-owned macOS capture reproduced the exact stage: exit 101 after 7.90s test time, `timed out after 6s waiting for: message reaches terminal status`, with message `msg_52b0f5ed0008`. Its row remained `target_resolved`, delivery attempts were `1`, and no delivery timestamp existed. See `raw/F16-R2-macos/repro/delivery-timeout-31624-1-msg_52b0f5ed0008.json`.
- An independently observed selector pass (1/1 in 6.94s) is retained as a non-reproduction only; it does not classify the red as flaky or green. See `raw/F16-R2-macos/reproduction.md`.

## Root-cause assessment

All callers of the wait and setup helpers were inspected before editing. The owned test calls the same six-second delivery wait used by two `send_001_fake_worker` tests. `quick_start_launched` accepts `all_workers_spawned` for several degraded statuses but does not verify coordinator heartbeat or delivery-path readiness.

The receipt supplies the first anomaly: the message was injected into the physical pane `%0` for `a`, while state still claimed `a` used live pane `%1`, whose capture showed `TEAM_AGENT_FAKE_READY agent=b`. The coordinator was alive but only at tick `1`; leader-receiver events are for a distinct report-result message and do not explain the worker delivery timeout. The correction is therefore an owned-test barrier on the existing fake-worker READY marker for both panes, before tuple corruption/send. The saved red run is the bypassed baseline; no sleep, retry, timeout inflation, framework-wide behavior, state machine, or weaker routing assertion is introduced. See `raw/F16-R2-macos/source-manifest.md`.

## Validation

- Uncontrolled baseline without the marker barrier: exit 101, 0 passed / 1 failed / 67 filtered, with the CI-observed timeout and durable receipt.
- Restored correction: owned selector rerun once on the exact final bytes, exit 0; 1 passed / 0 failed / 67 filtered; 6.97s test time.
- `cargo check --locked --package team-agent`: exit 0, with existing warnings.
- `rustfmt --edition 2021 --check crates/team-agent/tests/e2e/cases/wleak_worker_delivery_socket_leak_contract.rs`: exit 0.
- `git diff --check`: exit 0.
- Fresh Grok detached unit `gb-ad7c221715fc-f16-wleak-a1`: preflight passed, but sync refused the then-present controlled-not-ready mutation (`source tracked bytes are dirty`). No remote command/exit receipt exists; classify apparatus-unjudgeable and do not retry this unit.

## Required next action

The controlled-not-ready attempt in `raw/F16-R2-macos/tooth/controlled-not-ready.log` failed early at its own setup assertion (`controlled not-ready tooth setup raced past the worker READY marker`), not at `message reaches terminal status`; it is metadata-only and does not satisfy the required deterministic causal tooth. The temporary mutation was removed and the final bytes match the committed readiness barrier, but without a valid controlled red tooth this repair cannot be judged ready. Do not dispatch GitHub; F17 remains blocked.
