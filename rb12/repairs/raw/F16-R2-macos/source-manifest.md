# F16 R2 source and helper inspection

## Exact source manifest at reproduction head

```text
c95e581e6b2a3dfa5af3ef2cd30252ed5d32618db3dc3b359e149e9085d2df3f  crates/team-agent/tests/e2e/cases/wleak_worker_delivery_socket_leak_contract.rs
0fc9964f3f8c6ab2f90d0ff6f1498bcba00ecdb01d3d4ed580a3e46b98ffcffd  crates/team-agent/tests/e2e/framework.rs
baff138b58dc832c28a08bc625382d0d850751904a0d433cf2083495651bbda6  crates/team-agent/src/messaging/delivery.rs
```

## Calls and readiness facts inspected before any edit

- The CI-observed wait call is in the owned WLEAK test at lines 65-77; its predicate accepts only `delivered` or `queued_pane_missing` and keeps the six-second bound.
- `wait_for_delivery_or_panic` is called by three E2E sites: the owned WLEAK case plus two `send_001_fake_worker` cases. The helper records row, all/message events, coordinator pid/health/heartbeat, target agent state, physical tmux tuple/capture, and resource ledger only on failure.
- `quick_start_fake` has 60 call sites and `quick_start_launched` has 57 call sites under `crates/team-agent/tests/e2e`.
- `quick_start_launched` treats `ok=true` as ready; for `leader_receiver_unbound`, `pending_tool_load`, and `pending_session_capture`, it also treats `/readiness/all_workers_spawned=true` as sufficient. It does not assert coordinator heartbeat or worker delivery-path readiness.
- The framework does expose coordinator heartbeat facts only through the timeout snapshot (`.team/runtime/coordinator_tick.json`), not as an existing success-path readiness barrier usable by the owned test.

## Conclusion

The clean-target timeout receipt selected the concrete correction: after quick-start, wait for both intended fake-worker panes to show their existing `TEAM_AGENT_FAKE_READY agent=<id>` marker before corrupting the cached tuple and sending. This is an owned-test readiness barrier; it neither changes product delivery nor adds sleep, retry, or timeout. Removing that wait is the recorded red baseline in `repro/`; restore the exact wait and rerun the owned selector once green.
