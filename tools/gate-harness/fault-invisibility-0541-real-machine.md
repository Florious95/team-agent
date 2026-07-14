# FAULT_INVISIBILITY_0541_REAL_MACHINE

Marker: `FAULT_INVISIBILITY_0541_REAL_MACHINE`

Reference:
- `.team/artifacts/fault-invisibility-locate.md` §11 (User-visible
  acceptance) + §9 RED 5 (real-machine gate).
- `crates/team-agent/tests/fault_invisibility_0541_contract.rs`
  `real_machine_fault_invisibility_gate_is_declared` (RED5).

## Purpose

Declare the minimum real-machine acceptance gate for the 0.5.41 fault
invisibility slice: after a host reboot, coordinator death, tmux server
crash, or worker provider exit, the normal user commands
(`team-agent status`, `team-agent diagnose --json`, `team-agent
restart`) must report the runtime staleness truthfully — no cached
`工作` where the underlying pane/pid/session bindings are dead.

## Signals covered

- `runtime_bindings_stale_after_boot` (§6.1/§6.2 heartbeat
  host_boot_id) surfaced by both `status` and `diagnose`.
- `TEAM_AGENT_TEST_HOST_BOOT_ID` override lets the gate simulate a
  reboot without actually rebooting the host.
- `worker_provider_exit_marker` (§6.4 D-m) — worker wrapper leaves the
  pane alive at a shell; the capture-tail marker beats pane_pid
  falsely reporting the shell as the provider.
- `team-agent restart` remains the sole canonical recovery path
  (§11 acceptance contract — no new command introduced).

## Minimum matrix (0.5.41)

Only these narrow production shapes are in scope for the first car
(§11 acceptance contract). Later cars widen coverage.

- Invocation: leader pane bang injection running `team-agent status`
  and `team-agent diagnose --json`.
- Socket: private tmux socket (`-S /private/tmp/tmux-*/ta-*` or
  `-L ta-*` selected via team state), NOT the default socket.
- Provider: at least one worker under the 0.5.39 shell wrapper.

## Assertions (real-machine gate)

Given a private-socket managed Team Agent team with at least two
workers and a managed leader pane, then EITHER:

1. Simulate a host reboot: kill the team tmux server and coordinator,
   then export `TEAM_AGENT_TEST_HOST_BOOT_ID=new-boot` (contract path
   equivalent to a real reboot). Run:
   - `team-agent status --team <team>` — no worker rendered as `工作`;
     stale badge present.
   - `team-agent diagnose --team <team> --json` — issues list
     contains `runtime_bindings_stale_after_boot` + `team-agent
     restart` hint.
2. OR: kill only one worker's provider while leaving the pane alive
   (the wrapper falls back to an interactive shell, prints the worker
   exit marker):
   - `team-agent status --team <team>` — that worker renders
     non-working (`错误`/`未知`), other workers unaffected.
   - `team-agent diagnose --team <team> --json` — issues list stays
     non-stale for the healthy workers.
3. In either branch, `team-agent restart` remains the sole recovery
   path and, once completed, `status` returns to healthy/non-stale.

## Deliberately out-of-scope for first car

- Automatic recovery (locate §7 non-goal: read-only commands must not
  start/stop/rotate/kill anything).
- Provider-crash-as-idle: `未知` is acceptable, `空闲` is not.
- launchd/watchdog supervision.
- External tmux 3.6a broadcaster mitigation beyond the already-
  observable `tmux_server_crashed` classification (0539 §11.1 B).

## Consumers

- `fault_invisibility_0541_contract::real_machine_fault_invisibility_gate_is_declared`
  scans this file for the marker and required tokens
  (`FAULT_INVISIBILITY_0541_REAL_MACHINE`,
  `TEAM_AGENT_TEST_HOST_BOOT_ID`,
  `runtime_bindings_stale_after_boot`, `worker_provider_exit_marker`,
  `team-agent restart`, `status`, `diagnose`) as the source contract
  that this gate has been declared.
- Later cars promote this declaration into an executable
  `.team/artifacts/gate-harness/*.sh` real-machine harness. That
  upgrade is deferred per §11 acceptance contract for 0.5.41.
