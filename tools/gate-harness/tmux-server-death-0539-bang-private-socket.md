# TMUX_SERVER_DEATH_0539_BANG_PRIVATE_SOCKET

Marker: `TMUX_SERVER_DEATH_0539_BANG_PRIVATE_SOCKET`

Reference:
- `.team/artifacts/tmux-server-death-locate.md` §7 Slice 5 / §11.3 (revised
  first-car RED additions) — minimum production shape gate.
- `crates/team-agent/tests/tmux_server_death_0539_contract.rs`
  `minimum_bang_private_socket_bare_add_agent_gate_is_declared` (RED4).

## Purpose

Declare the minimum production-shape acceptance gate that the 0.5.39
first car must cover: managed leader pane bang injection running the
bare `add-agent` command against a private tmux socket, asserting that
the tmux server-death blast-radius stays bounded to the new agent only.

## Minimum matrix (0.5.39 first car)

Only this narrow production shape is in scope for the first car (§11.2
scope cut — no broad upstream fuzz matrix). Later cars widen coverage.

- Invocation shape: managed leader pane, bash bang injection (`!<cmd>`
  form the user actually types in Claude/Codex), NOT subprocess.
- Command: bare `add-agent <new-agent> --role-file <path> --workspace .
  --team <team>`. No `--no-display`, no `--json` — the shape task#6
  missed.
- Socket: **private** tmux socket (`-S /private/tmp/tmux-*/ta-*` or
  `-L ta-*` selected via team state), NOT the default tmux socket.
- Topology: existing team with at least three pre-existing worker
  panes so worker MCP `stdin_eof` cascading is observable.

## Assertions (real-machine gate)

Given a private-socket managed Team Agent team with ≥3 live workers and
a managed leader pane, the leader pane runs `send-keys`-injected bare
`add-agent <new> --role-file <path> --workspace . --team <team>`:

1. tmux server still exists on the private socket after the operation.
2. Team session still exists on the private socket.
3. All pre-existing worker panes still exist and remain injectable.
4. **No pre-existing worker emits `mcp.server_exit` with
   `reason=stdin_eof`** — the QA report's core signature of blast-radius
   overflow.
5. **No `coordinator.session_missing` event** — coordinator sees the
   team session for the full window.
6. If the new agent fails to start, only the new agent's spec/state
   rows are rolled back — old agent rows are byte-identical before and
   after the failure.

## Deliberately out-of-scope for first car

- Broad external-broadcaster/control-mode attach/detach storm fuzz
  (§11.1 E: external control clients are a documented non-goal).
- Full A/B/C/D/E case matrix — first car narrows to Case A only.
- Subprocess-only variants of `add-agent` (the existing E2E entries
  already cover those; they are not equivalent to the bang shape).

## Consumers

- `tmux_server_death_0539_contract::minimum_bang_private_socket_bare_add_agent_gate_is_declared`
  scans this file for the marker and required tokens (`send-keys`,
  `add-agent`, `--role-file`, `--workspace`, `--team`, `private`,
  `mcp.server_exit`, `coordinator.session_missing`) as the source
  contract that this gate has been declared.
- Later cars promote this declaration into an executable
  `.team/artifacts/gate-harness/*.sh` real-machine harness. That upgrade
  is deferred per §11.2 scope cut.
