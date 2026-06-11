# Changelog

## 0.3.13

- **Copilot is now end-to-end usable** as both a leader and a worker provider:
  - The Copilot startup `Confirm folder trust / Do you trust the files in this folder?` prompt is recognised and auto-confirmed by the framework, so a fresh Copilot leader/worker boots all the way to ready instead of staying parked on the trust screen.
  - The leader's owner / receiver attribution is correct under a Copilot leader — previously `leader.provider=copilot` but `leader_receiver.provider=codex` / `team_owner.provider=codex` because the quick-start seed path defaulted to `codex` when the launcher couldn't observe the leader yet. The unified attribution path (E22) now covers this sixth call site so the seeded receiver/owner provider matches the real leader provider.
  - `team-agent shutdown` on a Copilot-led team no longer misidentifies the leader pane — the rediscovery path that already understood codex/claude leaders now also understands `copilot`, so bare shutdown reliably spares the leader instead of killing it.
- **`team-agent restart` no longer takes the whole team down when one worker can't resume**: the spawn loop now isolates per-worker failures — a worker whose resume fails is marked failed and the other workers continue. `restart` exit code now reports `partial` when some workers came up and some didn't, instead of a single all-or-nothing `ok`/`fail`. A single failed `--resume` no longer kills the shared tmux session and starves the next spawn.
- **Nested team state is preserved on merge**: when a child team launches inside a workspace that already hosts a team, the parent team's `teams` map is preserved through the launched-state merge instead of being overwritten. A user-supplied `owner_team_id` is now ignored with a warning rather than silently honoured, so it cannot point a child team at the wrong owner.
- **Honest exit codes in two more places**: a `team-agent shutdown` against a workspace whose coordinator was already absent now reports `ok` cleanly instead of falsely reporting `partial`; `remove-agent` and friends now return `ok: false` envelopes on Refused outcomes instead of returning `ok: true` for a no-op.

## 0.3.12

- **Coordinator now detaches from the launching terminal**: the coordinator daemon process is spawned with `setsid` so it becomes its own session/process-group leader instead of inheriting the launcher's. Closing the terminal that ran `team-agent quick-start` (or disconnecting an SSH session) no longer sends `SIGHUP` to the coordinator and no longer kills it as a side effect — common cause of "coordinator died after I closed the window" on WSL/SSH workflows.

## 0.3.11

- **Message delivery now surfaces a degraded status when the coordinator is not alive**: `team-agent send` (and the internal MCP send path) previously returned `accepted` even when the coordinator was dead, leaving messages silently stuck. The send path now returns an explicit degraded status instead of falsely reporting acceptance, so the leader can tell immediately that the coordinator needs attention rather than waiting on a delivery that will never happen.
- **Coordinator crashes now leave a durable post-mortem marker**: a panic inside the coordinator tick is now caught and written to `coordinator.tick_panic` with the captured backtrace, so `team-agent diagnose` and on-disk inspection can see where the coordinator died instead of finding only an absent process. Combined with the 0.3.10 "no silent self-exit" change, every coordinator failure mode now produces an evidence trail.

## 0.3.10

- **`team-agent restart` no longer resurrects dead worker sessions (#264, E20)**: the resume gate was only running a transcript-existence check for `codex` workers — `claude`, `claude_code`, and `copilot` workers were promoted to `Resumed` purely on the presence of a stored `session_id`, even when the provider had no live transcript to resume against, producing a zombie window that looked alive but was attached to nothing. The transcript-existence check now runs for every provider that has a known transcript root, so a stale session id falls through to refused-or-fresh-by-policy instead of being silently honored.
- **Restart readiness gate (E20 C②)**: after relaunch, `restart` now waits for each worker to reach a ready state before reporting the team restart as complete; partial relaunches no longer silently look successful while leaving workers stuck mid-startup.
- **Coordinator no longer silently self-exits** on transient errors and **messages no longer silently disappear** in delivery race conditions — both paths now surface failures through the event log instead of swallowing them.
- **`--dangerously-bypass-approvals-and-sandbox` / `--dangerously-skip-permissions` inheritance is correct for internal MCP tools (#232)**: when the leader is started with a bypass flag, internal Team Agent MCP tools (orchestrator send/result/etc.) inherit the bypass correctly so they no longer trigger approval prompts that the user already opted out of.
- **`team-agent shutdown` under a Copilot-launched leader no longer kills the leader window**: the leader-pane rediscovery path now recognises `copilot` as a valid leader command (it previously only looked for `codex`/`claude`), so the shutdown-protection extension correctly identifies and spares the Copilot leader.

## 0.3.9

- **`team-agent restart` standard usage actually works (RED-2-STILL, P0 hotfix)**: the restart entry gate now resolves the team workspace through `canonical_run_workspace` before checking for a spec, so the documented form `team-agent restart .` (or `team-agent restart <workspace>`) finds the spec under `.team/runtime/<team_key>/` instead of falsely reporting "no spec" because it was looking at the raw workspace argument. The second-layer guard from 0.3.8 (067f78f) is preserved and still catches deeper misses.
- **`team-agent copilot` is now a first-class entry command**: `team-agent copilot [args...]` launches or attaches a tmux-managed Copilot leader the same way `team-agent codex` / `team-agent claude` do; the provider mapping, help text, and candidate-list now surface Copilot. Copilot leader processes are also started with `COPILOT_DISABLE_TERMINAL_TITLE=1` so the leader pane's tmux/terminal title stays stable instead of being repeatedly overwritten by Copilot.
- **Task results no longer double-deliver to the leader**: `report_result` previously took both a direct-inject path AND the normal delivery path, so a leader could see the same completion reported twice. The direct-inject path is now gated to deliver-failure only (`if !outcome.ok`), so a successful result is delivered exactly once via the normal path.

## 0.3.8

- **Shutdown safety (E12, P0)**: a bare `team-agent shutdown` now spares the leader pane by reading the state.json leader anchor and gating shared-socket `kill_server` — closing a three-time recurrence where shutdown would kill the leader window itself. The runtime now refuses to issue a server-wide kill against a socket carrying a registered leader anchor for any team.
- **Copilot end-to-end is now actually usable** (three pieces together):
  - `npx @team-agent/installer install` now installs the `team-agent` skill into the Copilot skill directory by delegating to a single canonical Rust path (`team-agent install-skill`); JS-side install code is gone — one source of truth, no double-install drift.
  - Copilot worker session attribution is correct under restart: when a worker has an `expected_session_id`, the runtime point-queries the `~/.copilot/session-store.db` sqlite by id (sessions.id == expected) instead of grabbing the latest-by-cwd row, so a worker no longer accidentally inherits the leader's Copilot session when both share the same cwd. When `expected_session_id` is unset, the runtime refuses to fall back to "latest at cwd" — it returns empty rather than promote a session that could belong to anyone.
  - Copilot leader / worker provider binding no longer crosses streams under leader + worker started in the same workspace.
- **Restart spec gate (RED-2, P0)**: `team-agent restart` now reads the runtime spec from `selected.spec_path` (the read-order B canonical path under `.team/runtime/<team_key>/`) instead of the user-visible workspace directory — so restart correctly finds the spec even when the user-visible directory was never populated, and behaves the same as quick-start about where the team spec lives.
- **`team-agent attach-leader`** is wired and surfaces the canonical attach command in human-readable output; `quick-start` ready output now includes the `attach_commands` table so attaching the leader pane does not require digging into runtime state.
- **Leader pane env preflight (RED-4 batch)**: a stale or invalid `TMUX_PANE` env coming into quick-start/restart is now caught at entry — empty pane probe output, absent env, invalid pane references are handled in turn; transient unknowns are degraded rather than promoted to fatal; warnings are persisted into team logs for after-the-fact diagnosis.
- Multi-socket transport probe: the tmux backend now scans all standard socket roots (`/tmp/tmux-<uid>`, `$TMPDIR/tmux-<uid>`) instead of a single hard-coded `/tmp` root, so workspaces using a non-default `TMPDIR` can be located.

## 0.3.7

- Runtime team spec is now single-sourced under `.team/runtime/<team_key>/` instead of being copied into user-visible workspace paths; `team-agent restart` rebuilds the runtime spec from your role docs each time (so edits to `agents/<role>.md` take effect on restart without manual regen), and `team-agent add-agent` no longer copies the role file into the platform directory — fixes a self-truncation bug where add-agent would zero out the source role file.
- Claude worker session attribution is now reliable for interactive workers (primary path verifies against the provider's project transcript; a fallback path covers the case where the transcript hasn't landed yet); `team-agent restart` with a worker that has no stored session id now refuses by default rather than silently spawning a blank worker — pass `--allow-fresh` only when you explicitly accept a fresh context.
- `npx @team-agent/installer install` now also installs the `team-agent` skill for Copilot, so a Copilot leader can actually use Team Agent skills after install (was missing before); CLI error-path guidance is unified across commands to the three-line `ERROR / ACTION / LOG` form.
- Coordinator tick degraded-monitor pattern (N36 face decouple): runtime-prompts / sync-agent-health / runtime captures each catch their own errors, emit a `*.failed` event, and continue — so a single capture failure no longer kills the whole tick or blocks delivery. `provider.classify.unsupported` emits at most once per check_key state-change (P4 dedup) instead of looping.
- Status `runtime` block: `team-agent status --json` now carries `runtime: { coordinator: { ok }, undelivered, hint? }`. `hint` only appears when the coordinator is down AND there is an undelivered backlog (anti-nag).
- Lifecycle quick-start validates the active leader pane env at entry; a stale `TMUX_PANE` pointing at a Dead pane now fails closed with the N38 three-line `ERROR / ACTION / LOG` form instead of silently retargeting.
- Leader terminal protection (B5) cross-socket coverage: protected-pane extension now reads three sources (session-prefix scan + `state.json` leader anchor + cross-socket query via the user's tmux endpoint), so leader panes on the user's default tmux socket survive workspace-socket shutdown.
- COPILOT v2 phase-1: argv + windowtitle on launch (`Q4 P0`); MCP residual scan + sqlite session capture + BYOK env wiring; `caps.fork=false` is an honest `CapabilityUnsupported` refusal (no silent fresh-spawn fallback); per-worker model override via role `model:`.
- Doctor legacy compatibility: a legacy `team_invalid` spec no longer hard-errors `doctor`; it reports a degraded status row and lets diagnosis continue.
- Add-agent / remove-agent / start-agent semantic distinction documented in SKILL.md (runtime add = `add-agent` one command, do not shutdown/restart to add a worker); subscription-tier `team-agent profile init` prelude added; private tmux socket + `attach_commands` documented; provider capability matrix table covers claude/codex/copilot/gemini_cli/fake.
- B-2 ownership: 2 weak-locked black-box contracts left intentionally pending in `tests/`, tracked for the next fixup batch.

## 0.3.6

- Hotfix: Linux x64 binary was built against the runner's glibc 2.39, leaving it unusable on every distro shipping older glibc (Ubuntu 22.04 / 20.04, Debian 12, WSL defaults, etc.). The release pipeline now targets `x86_64-unknown-linux-musl` with full static linking — no GLIBC_ version symbols and runs on any modern Linux. Inherits the same fix for Linux users on 0.3.0 → 0.3.5 (all earlier `cli-linux-x64` packages were affected; upgrade is required to run on Linux).
- Release pipeline: pre-publish assertion that the Linux artifact is statically linked AND carries zero `GLIBC_` version symbols (reverting the target back to `linux-gnu` will trip the gate before any package reaches npm).
- Installer: `npx @team-agent/installer install` now runs a `team-agent --version` post-install smoke and exits non-zero with a three-line `ERROR/ACTION/LOG` diagnostic when the freshly installed binary fails to load — catches loader-level failures (glibc mismatch, cpu mismatch, corrupted download) at install time instead of on first use.
- CI: workflow checkout actions bumped to `v5` (no behavior change).
- Re-includes ef7ab3d's permission-tier downgrade real-machine coverage.

## 0.3.5

- Faithful-port fixes vs Python 0.2.11 (#264 D1-D9): codex `developer_instructions` triple escaping, MCP `tool_timeout_sec=600`, profile `--profile`/`codex_config` injection, worker `TEAM_AGENT_ID` env, fresh-launch cwd=workspace, identity-first system prompt, `runtime.fast` codex toggle, `team-{name}` session fallback, real `unset` of profile keys in the worker shell.
- State safety (A0): the per-team roster preserve now survives an active-team-key flip between a writer's load and save; session-capture fields are monotonic across stale-snapshot saves.
- Leader terminal protection (B5): a bare `shutdown` spares `team-agent-leader-*` sessions and the tmux server carrying them; the leader pane process tree joins the shutdown protection set.
- Fixed-failure batch (A-1..A-8): `collect` reports the real coordinator status; the takeover reminder honors the arm gate; `watch`/`status` panels read live store data (team filter, latest results, agent detail); a missing leader receiver no longer reads as attached; watcher retries go through the real delivery path; MCP `stuck_cancel`/`send`/`fork` contract fixes; coordinator start refuses incompatible `team.db` schemas.
- Tick & shutdown performance (P1-P7, PERF-6): bounded transcript tail reads (128KB), head-bounded session-capture reads + candidate cap 300, steady-state ticks no longer rewrite `state.json` (tick counter moved to its own metadata file), change-driven compaction events, one pane snapshot per tick, one process-table snapshot + batched TERM/grace/KILL in shutdown (ps forks 10-15 -> 4), orphaned coordinator self-termination, `tick_error` signature dedup.
- Observability (swallow batches 1-4): probe/query/exit/config failures now emit events with non-null errors instead of silent empty results; corrupt provider-config JSON fails explicitly and never rewrites the user's file; MCP scope validation fails closed when the runtime state is unreadable.
- New provider: GitHub Copilot CLI (subscription-tier A-layer: BYOK env, auth hints, per-worker instructions overlay, sqlite session capture).
- Internal: constitution anchors N36-N39 and MUST-17 codified.

## 0.3.4

- Changed the default team display backend to `none`; set `display_backend: adaptive` in `TEAM.md` to opt in to adaptive display windows.
