# Changelog

## 0.5.9

- **DX: `report_result` integrity warnings for unverified success claims.** When `report_result` is called with `status: success` but the attached tests field contains no executed evidence (all entries are `not_run`, scalar values, or missing descriptions), the coordinator now accepts the call but returns a structured `warnings` array instead of treating the claim as verified. Callers can distinguish a verified success from a best-effort claim without parsing prose.
- **DX: `--to-name` failure uses a three-tier refusal taxonomy.** Resolution failures now return one of three structured error kinds: `team_key_not_found` (lists available team keys and notes if a spec name was given instead of a team key), `leader_not_attached` (distinguishes whether the caller is the owner team or a third party), or `workspace_no_state` (the target workspace has no coordinator state). Each kind carries enough context to act on without a follow-up `status` call.
- **DX: offline mailbox — `send --to-name` queues to disk when leader is unattached.** When the named target's leader is not currently attached, the message is written to an offline mailbox (`queued_until_leader_attach`) rather than failing. When the leader attaches (via `attach-app-server-leader` or equivalent), queued messages are replayed exactly once in arrival order. The response includes `delivery_status: queued_until_leader_attach` so callers know the message is safe without polling.
- **New command: `team-agent leaders`.** Discovers all host-leader registry entries for the current machine and classifies each as `LIVE`, `STALE`, or `AMBIGUOUS`. Useful for diagnosing multi-team or multi-workspace setups where multiple leader sockets may be registered. `--json` returns the full registry schema.
- **New flag: `send --to-leader`.** Sends to the unique live leader on the current machine by short name, without requiring the full `--to-name workspace::team/leader` address. Refuses with a structured error if the leader is ambiguous or stale, directing the caller to use `team-agent leaders` to resolve.

## 0.5.8

- **DX: `status` stale detection sourced from physical liveness, not lifecycle state.** When a worker's tmux pane is dead, `status` now marks that worker `stale: true` with `stale_reason: "pane_dead"` derived from a physical liveness probe — not from the coordinator's own lifecycle bookkeeping. This prevents stale agents from appearing live in status output when the coordinator has not yet observed the pane exit.
- **DX: `status` exposes `current_task` and heartbeat as structured best-effort observation.** The status response now includes a `current_task` field (the task currently assigned to the worker) and a heartbeat timestamp for each agent entry. These are best-effort observations from the coordinator's view — not authority — and are marked as such so callers can distinguish coordinator knowledge from ground truth.
- **DX: `--to-name` failure attaches structured advisory candidates.** When a named-address send fails to resolve the target, the error response now includes an `advisory_candidates` list of reachable named addresses on the same socket, ranked by recency. This lets the caller see what names are actually live without running a separate `status` call.
- **DX: session all-gone recovery chain surfaces structured short-circuit hint.** When all workers in a session have exited and `diagnose` or `status` detects the all-gone state, the response now includes a structured `recovery_hint` that describes the recommended recovery path (e.g., `restart --allow-fresh`) along with the team and session context — condensing the triage steps a human or agent would otherwise have to run manually.
- **DX: recovery-originated task assignment carries structured confirmation marker.** When a worker accepts a task via a recovery path (rather than a normal assign), the task envelope now includes a `recovery_marker` field identifying the recovery origin. This lets the receiving worker log and surface the recovery context without parsing the message text prefix.

## 0.5.7

- **DX: `remove-agent` rejection lists all required flags at once with a copyable command.** When `remove-agent` is refused because the worker is still running or the agent spec still exists, the error now includes every flag needed to complete the operation (`--force`, `--from-spec`, etc.) in a single copyable command line. Previously the error named flags one-at-a-time, requiring multiple retries to discover the full set.
- **DX: `send` default human output condensed to one line.** The default (non-`--json`) `send` output is now a single status line that reflects the actual delivery outcome. Fields that are permanently `None` for a given send shape are omitted rather than printed as `null`. The `--json` envelope is fully compatible with prior releases.
- **DX: `send` reminder text matches actual delivery status.** The harness reminder appended after a send now describes what actually happened (delivered, blocked, queued) rather than using a generic template that could contradict the status line above it.

## 0.5.6

- **Fix: `report_result` attributes to the current message turn as the primary source.** When a worker calls `report_result`, the coordinator now looks up the worker's current in-flight message turn first. If the turn is open and matches the worker's `agent_id`, the result is attributed to that message — superseding the previous fallback-first behaviour that could attribute results to a stale prior delivery. The injected result simultaneously arms the turn's `turn_open` marker so the next result call for the same worker correctly recognises a fresh turn.
- **Fix: historical backfill is capped to messages delivered after the latest known result.** The fallback path (used when no current turn is in flight) no longer considers messages delivered before the last confirmed result. This prevents a post-restart worker from having its fresh result silently attributed to an old, already-resolved delivery from a prior session.
- **Fix: `delivered` semantics remain strict throughout.** A message is marked `delivered` only when physical delivery to the tmux pane succeeds. The current-turn attribution fix does not relax this invariant — attribution and delivery status are updated independently, so attribution to the current turn does not imply delivery success.

## 0.5.5

- **Fix: `send` accepted ≠ delivered — semantic split with `delivery_status` field.** The response envelope now carries a `delivery_status` field (`delivered` / `blocked` / `queued_pane_missing` / …) separate from the top-level `ok` / `status`. `accepted` means the message entered the queue; `delivered` means it reached the tmux target. Callers can now distinguish physical delivery from queue acceptance without parsing `reason`.
- **Fix: `--watch-result` observes initial delivery, not just result collection.** When a worker window is missing at send time, `--watch-result` no longer registers a result watcher before the message has been physically delivered. The response carries `"channel": "delivery_blocked"` and no `watch` key until delivery succeeds.
- **Fix: tmux target missing classified as recoverable `blocked`, not permanent failure.** `queued_pane_missing` is now a typed blocked status. The message stays in the queue and is replayed when `start-agent` repairs the missing window.
- **Fix: `start-agent` repair replays blocked messages with the same `message_id` (idempotent).** When a worker window is repaired, queued messages are redelivered using their original `message_id`. Replaying the same id a second time is a no-op — the coordinator deduplicates by id, so retry-safe callers receive exactly-once semantics.
- **Fix: leader notification delivery enforces socket boundary exclusivity.** Leader receiver delivery now requires that the target pane's tmux socket matches the recorded `leader_receiver` socket. Cross-server delivery (fallback to a pane on a different tmux server) is refused when a socket is on record, preventing silent delivery to the wrong server.
- **Fix: `fallback_pane` boundary enforced with loud refusal.** When a `fallback_pane` would cross to a different tmux server than the recorded socket, delivery is refused with a structured error rather than silently delivering to the wrong pane.
- **Fix: pending leader notifications replayed after `attach-leader` re-bind.** When a leader receiver is re-attached (via `attach-app-server-leader` or equivalent), any leader-bound messages that failed delivery while the receiver was absent are now replayed. `status` exposes these as `pending_leader_notifications` so operators can observe the replay queue before and after attachment.

## 0.5.4

- **Fix: Codex session identity crossbind — same-cwd multi-worker capture corrected.** When multiple Codex workers start in the same working directory, session attribution previously relied on recency and could assign a session to the wrong worker. Sessions are now attributed by reading the embedded `TEAM_AGENT_AGENT_ID` identity marker from the rollout transcript head. Real-machine acceptance: 8 same-cwd Codex workers each captured a distinct session, with `attribution_confidence: high` for all eight.
- **Fix: `restart` identity preflight and postflight checks.** Before spawning workers, `restart` verifies that each worker's stored `session_id` maps to a rollout transcript embedding that worker's own `agent_id`. A mismatch is refused with a structured `refused_session_identity_mismatch` response naming the affected worker, the stored session UUID, and the embedded identity. After spawning, a postflight check confirms the resumed transcript's embedded identity matches. Both checks surface `identity_ok` / `embedded_agent_id` fields in restart event logs.
- **Fix: `restart --allow-fresh` clears poisoned Codex tuple atomically.** When a worker is decided `fresh_start` due to an identity mismatch, both the root and team-scoped `session_id` / `rollout_path` fields are now cleared before the fresh process is spawned. Previously, the poisoned tuple persisted in state and `diagnose` continued to report `session_identity_mismatch` even after a successful fresh start.
- **Fix: `diagnose` and `status` expose session identity mismatch.** When a crossbind is detected, `diagnose` exits with code 1 and reports `issue: session_identity_mismatch:<worker_id>` with the `expected_agent_id`, `embedded_agent_id`, poisoned session UUID, and a remediation hint (`restart --allow-fresh`). `status` surfaces the same poisoned tuple fields for operator visibility.

## 0.5.3 (Windows-native, Preview)

- **Windows-native transport (`--backend conpty`) real-machine surface.** Full end-to-end delivery on native Windows via ConPTY + named-pipe shim. `team-agent quick-start --backend conpty --team <name>` spawns the coordinator daemon, which owns the `windows-shim.exe` process for the (workspace, team) tuple. `team-agent send <agent> "<msg>"` returns `ok=true` with `message_status: "accepted"` and a `message_id`. `shutdown` reaps the shim via `platform::process::terminate_pid` and emits `conpty_shim` audit action. **Real-machine six-check on Windows 10.0.26200 (batch9-final = 81a360c): 5/6 PASS + 1/6 partial** (`peek` needs the coord daemon's shim connection which CLI processes can't share yet — a follow-up refinement).
- **Packaging still declares Windows as `PreviewCompileOnly`, NOT Native.** Constraint from Batch 7-9 gate reports: promotion to `Native` requires 6/6 real-machine PASS. `peek`'s partial status blocks that promotion; the 0.5.3 shipping surface is honest about being a Preview.
- **Subscription-provider gate deferred.** Windows real-machine `codex` (`codex-cli 0.120.0`) and `claude` (`2.1.170 Claude Code`) are installed but not authenticated on the SSH host. Real-provider `send`+`report_result` round-trip verification requires operator login; documented as "待用户" per leader constraint.
- **Bug: F4 (Phase 1b shim client-reuse race) fixed.** `windows-shim.exe`'s accept loop now treats `ERROR_PIPE_CONNECTED (0x80070217)` as success and calls `FlushFileBuffers` + `DisconnectNamedPipe` before recreating the pipe instance. Fixes the "shim dies after first Hello disconnect" symptom.
- **Bug: F5 coordinator daemon on Windows no longer falls back to tmux.** `run_daemon` derives `team_key` from `--team` CLI arg (Batch 9) or `state.active_team_key` fallback (Batch 7). Enables the daemon to build a `ConPtyBackend` instead of the tmux-fallback path that broke every MCP-facing operation.
- **Bug: F6 `state.transport.shim` block preserved across merges.** `state::persist::apply_persist_merge_contract` now calls `preserve_transport_shim(incoming, latest)`. Fixes the "shim block silently dropped by downstream state saves" symptom.
- **Bug: F7 daemon-shim coexistence gap fixed.** Shim ownership moved from `quick-start` (one-shot) to `coordinator::conpty_shim::ensure_shim_running` (daemon-owned). Windows shim + coord daemon spawns now set `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` creation flags so both truly outlive their SSH-parent process trees.
- **Bug: F8 `quick-start` seed-state ordering fixed.** Coordinator daemon now receives `--team <name>` on its CLI (via `start_coordinator_with_team`), so `quick-start` no longer needs to pre-seed `state.active_team_key`. Fixes the "existing runtime, use restart" branch triggering on fresh Windows quick-start.
- **RED-first contracts locked.** 8 RED tests across Batches 6-9 guard the shim reuse race, the state-merge preserve, the coordinator ownership shape, the reconnect API, the C-3 stale event family, the detached-process spawn flag, the launch-flow seed-state absence, and the coordinator `--team` argument forwarding.
- **CR anchors held.** C-1 (no silent tmux fallback), C-3 (typed stale events for shim death), C-4 (sha256 CI==Remote reconciliation), C-6 (`platform.terminate_force_only` audit event) all preserved and real-machine verified.

## 0.5.2

- **Added: `--backend conpty` transport for Windows-native ConPTY sessions.** A new `--backend` flag lets you explicitly select the transport backend: `tmux` (default, unchanged behaviour) or `conpty` (Windows ConPTY via named pipe). The full chain ships: a portable named-pipe control protocol (`conpty-transport`), a ConPTY shim binary (`win-conpty-phase0`), and a ConPTY backend in the coordinator (`conpty` module). The factory (`transport_factory`) assembles the right backend from persisted state at startup; on non-Windows hosts it returns a typed `MuxUnavailable` rather than silently falling back. Covered by factory guard suite (C-1/C-4/C-5/C-6), pipe-token persistence guard, and ConPTY end-to-end fake-worker tests.
- **Added: `attach-app-server-leader` subcommand for app-server leader hosting.** Codex app-server sessions can now act as the Team Agent leader host. The new subcommand wires a live Codex app-server pane as the leader receiver, writing the transport-kind tuple atomically and advancing the epoch. MUST-12 is updated: delivery paths are read-only with respect to ownership; the sole ownership-mutation entry point is the explicit `attach-app-server-leader` CLI path.

**Note on CI baseline:** The historically-known red test `leader_bound_delivery_must_target_bound_leader_pane_not_missing_leader_window` was absent from the merged integration run (0 failures in --tests). CI hermetic run will confirm whether it remains absent or resurfaces; result will be noted in the release report.

## 0.5.1

- **Added: `send --to-name` resolves stable workspace/team/agent or leader names to the current live pane.** Routes a message to a live tmux pane by stable name (`<workspace>::<team>/<agent>` or `leader`) without needing to look up the pane ID. The MVP assumes a trusted local caller; no auth gate is applied. Design includes 7 architectural constraints and is covered by 13 unit tests; 12/12 real-machine send scenarios passed.

## 0.5.0

This release completes a six-phase internal refactor that was carried out entirely behind the existing public API surface. Every gate (Phase A–F + 12-item subscription gate + leader gate) passed before shipping.

**Six-phase refactor summary**

- **Phase A — Lifecycle transaction lock.** Coordinator state transitions (start/stop/restart) are now serialized through an explicit lock, eliminating the class of races where two concurrent operations would corrupt worker state.
- **Phase B — Persist merge contract contraction.** The persistence layer's merge surface was narrowed: callers can no longer accidentally clobber in-flight state by passing a partial struct. The contract is now a typed diff rather than a full replacement.
- **Phase C — Scanner split.** Provider session detection was split into per-provider scanner modules (Claude, Codex, Copilot). Each scanner owns its detection heuristics; the coordinator no longer carries interleaved provider-specific branches.
- **Phase D — Provider wire single-source.** All provider launch strings (binary names, env vars, session-id flags) are now defined in one place and injected at wire-up time. Duplicate definitions across start/restart/health paths have been removed.
- **Phase E — Abnormal step modularization.** The "abnormal tick" path (provider died unexpectedly) is now a standalone step module with its own entry/exit contract, making it independently testable and easier to extend.
- **Phase F — Abnormal-exit transcript-error single gate + recency + CLI 2.1.181 assistant-shape adaptation.** Detecting that a provider exited abnormally (vs. graceful stop) now goes through a single gate that checks transcript recency and is adapted to the Claude CLI 2.1.181 `assistant`-role message shape.

**Bug fixes**

- **Fixed: `doctor` failed to parse paths containing spaces.** The `doctor` subcommand now correctly tokenises space-embedded paths, preventing false-negative health checks on non-standard install locations.
- **Fixed: `reset` spawn failure is now fail-closed.** Previously, if a coordinator failed to spawn during `reset`, ownership was silently dropped. It now hard-fails and leaves the team in a safe stopped state.
- **Fixed: `restart` correctly rehydrates worker roles.** After a `restart`, workers that had a custom `--role` are now restored to that role instead of falling back to the default.
- **Fixed: `send --pane` argument parsing regression (introduced in 0.3.26).** Pane targeting was silently ignored after an internal refactor; this is now restored.

**MCP-CYCLE shape baseline**

The strict MCP-CYCLE shape is a known baseline as of 0.4.10. The functional PASS evidence chain (inbox → event → collect → notify) is complete and has not regressed in this release.

## 0.4.11

- **Fixed: starting a worker after a failed stop no longer opens a duplicate window.** If `stop` fails, Team Agent now refuses to `start` the same worker until the stop completes, preventing a second window from opening alongside the stale one.
- **Fixed: each `team-agent claude` launch now gets its own independent session.** Previously, a second launch could attach to an existing session instead of starting fresh; each invocation now creates a distinct session.
- **Improved: the generic teammate system prompt is leaner.** The runtime contract section was trimmed from 30 lines to 16, keeping only the essential MCP communication rules and making it easier for teammates to reach the key requirement: all replies must go through MCP tools.

## 0.4.10

- **Added: workers can now run at a configurable effort level.** Set `effort:` in a role's section of `TEAM.md` or the role document, and Team Agent passes the right flag to the provider. Supported values follow the provider's own effort scale.
- **Improved: worker lifecycle now tracks five distinct runtime states.** The new `WorkerRuntimeState` enum — `Pending`, `Starting`, `Running`, `Stopping`, `Stopped` — gives the coordinator and status commands a precise view of where each worker is in its lifecycle, replacing the previous boolean running/stopped split.
- **Improved: foreground workers now run in their own process group.** Phase 1 of the foreground process-group work launches each worker in a dedicated pgrp, so signals and cleanup target the right processes and do not accidentally affect the leader.
- **Fixed: `CLAUDE_EFFORT` is now scrubbed from the worker environment.** The env-export path previously could re-introduce variables that had already been unset; the scrub now runs after the full environment is assembled, so effort and other control variables do not leak into worker subprocesses.

## 0.4.9

- **Fixed: Claude environment variables no longer leak into spawned subprocesses.** The leader launcher now unsets all `CLAUDE_CODE_*` variables before starting workers, covering both the managed tmux path and the shell wrapper path. The ExecProvider in-tmux path also clears the Claude environment block, and the removal order is corrected to run after `envs()` is called — closing the leak that caused Claude workers to inherit the leader's session environment.
- **Fixed: E2E tests no longer leave coordinator processes running after a test completes.** `TestWorkspace::Drop` now precisely cleans up coordinator processes, preventing port/socket conflicts between test runs.

## 0.4.8

- **Fixed: Claude workers now start with a clean session, never inheriting the leader's session ID.** Previously, spawning a Claude worker could accidentally reuse the leader's Claude Code session environment, causing the worker to capture the leader's transcript instead of its own. Claude workers now launch in a fully isolated environment with all session-related variables cleared.
- **Fixed: `restart` now reliably resumes workers that had existing conversations.** A worker that had chatted before a shutdown is now brought back with its full context. A never-used worker (no transcript, no interaction) is correctly started fresh — no `--allow-fresh` flag required.
- **Fixed: session capture state is now set atomically with the session tuple.** The capture state and session ID are now written together, preventing a window where one was set but the other was not, which could cause misrouted messages.
- **Fixed: `reset` now clears the worker's spawn epoch**, so a restarted worker cannot accidentally be matched against stale spawn records from a previous lifecycle.
- **Fixed: the `transcript-ready` state machine is now consistent.** Workers only transition to the ready state once their transcript file is actually present and writable, preventing premature delivery attempts.
- **Fixed: status now rejects ambiguous multi-team targets.** If `--workspace` matches more than one team, `status` now returns `ok: false` with `reason: team_target_ambiguous` instead of silently picking one.
- **Fixed: `start-agent` now clears the fresh tuple and guards against pending mismatches** when launching a worker that previously existed, preventing stale state from leaking into the new lifecycle.
- **Fixed: Claude child/team environment is fully cleaned on worker spawn.** All `CLAUDE_CODE_*` and team-scoped environment variables are unset before launching a Claude worker, closing the root cause of session bleed-through.

## 0.4.7

- **Improved: message delivery and provider wiring are now more reliable.** Provider-specific logic is unified in one place, so fixes and improvements apply consistently across Claude, Codex, Copilot, and Gemini. A bug where GeminiCli and Fake providers missed some delivery updates is now fixed.
- **Improved: `status --json` output is now compact by default.** The default output shows 7 top-level fields and 4 per-agent fields — just what you need at a glance. Use `status --detail` to get the full diagnostic view with all coordinator and message fields.
- **Added: `status` now reports a `ready` / `not_ready` summary.** A single field tells you whether the team is ready to work and, if not, lists the reasons why (for example, workers not yet spawned).
- **Improved: provider adapters are now split into per-provider files.** Claude, Codex, Copilot, and Fake adapters each live in their own file, making the codebase easier to navigate and extend.

## 0.3.28

- **Improved: teammate windows now use one layout manager.** Starting, adding, removing, and restarting teammates now share the same layout rules, so panes are placed more predictably and new layout changes do not need to be reimplemented in several places.
- **Fixed: pasted messages no longer rely on the old grace fallback path.** Team Agent now waits for the real submit signal instead of treating a briefly visible pasted token as good enough, which makes delivery confirmation stricter and more reliable.
- **Fixed: Copilot teammates keep the expected launch flags.** Copilot workers now preserve the intended bypass settings through the unified layout and launch path.

## 0.3.27

- **Improved: message delivery now uses one consistent path.** Team Agent no longer has a separate side path for one kind of pasted message, so delivery checks behave the same way across providers and avoid branch-specific surprises.
- **Fixed: leader delivery now verifies the real leader pane before sending.** This prevents rare cases where the system could mistake a teammate window for the leader and send messages back into the wrong place.
- **Fixed: Copilot teammates keep the expected bypass flags during launch and delivery checks.** Teams using Copilot now preserve the intended startup flags instead of drifting into a stricter or inconsistent prompt flow.

## 0.3.26

- **Fixed: on a fresh Mac, the `team-agent` command was not found after install.** The installer now places it in a directory already on your `PATH`, so it works immediately.
- **Fixed: shutting down a team could close the leader terminal.** Shutdown now only closes worker windows and leaves the leader intact.
- **Fixed: starting a new teammate could steal the leader terminal focus.** New workers now open in the background.
- **Fixed: in rare cases the framework could mistake a worker window for the leader, causing messages to loop back.** The framework now refuses to rebind the leader to a registered worker pane.
- **Improved: message delivery diagnostics.** When delivery is retried, the framework now logs what it saw on screen, making it easier to diagnose delivery issues. It also uses the proven token-consumption gate for the Codex paste-prompt path, so submit verification is more reliable.
- **Added: send messages directly to any tmux pane with `team-agent send --pane`.** This works across teams for cross-team communication.

## 0.3.25

- **Fixed: adding a teammate to a running team could leave it unusable and impossible to remove.** In some cases, add said the teammate already existed while remove said it was unknown, because the two commands were reading different copies of the team record. They now use the same record, so add, re-add, and remove agree with each other. A failed add no longer leaves a half-created teammate behind.
- **Fixed: a new teammate added to a running team might not receive messages.** The first message could land in the new teammate's input box without being submitted. Team Agent now verifies that the message is submitted and the teammate starts working.
- **Fixed: a new teammate could be opened on top of an existing teammate's window.** Adding a teammate used to split into another teammate's window in some cases. New teammates now always get their own window.
- **Fixed: an idle teammate could be wrongly shown as busy or stuck.** Older versions guessed busy or idle state from screen text, which could be fooled by leftover text from a completed task. Team Agent now uses the actual conversation record to decide whether a teammate is working or idle.
- **Fixed: a misleading "has blockers" warning right after install.** The installer self-check could run doctor in an empty folder and mistake "no project here" for a real blocker. It now separates real problems from an empty-folder check.

## 0.3.24

- **Fixed: after pasting text into a teammate's input box, pressing Enter could sometimes do nothing and the message was not actually sent.** Previously, Team Agent could paste the text and press Enter immediately, before the pasted content had fully appeared in the teammate's interface. That left the message sitting in the input box instead of sending it. Team Agent now waits until the pasted content is visible before pressing Enter, so pasted messages are delivered reliably.
- **More reliable message delivery and team bookkeeping.** Team Agent is steadier when a team is closed and reopened, when multiple teams share the same folder, or when windows move around. Message addressing and team state records now line up more consistently, reducing edge-case delivery failures.

## 0.3.23

- **Fixed: closing a team and reopening it could lose a teammate's conversation, forcing a fresh start.** Previously, after you shut a team down and restarted it, a teammate that had already been chatting could be wrongly judged as "nothing to resume" — even though its full conversation history was safely saved on disk. You were then pushed to start that teammate over from scratch, losing its context. Now the system recognizes the saved conversation and brings the teammate back where it left off, so a normal close-and-reopen keeps everyone's context.
- **Added guidance for running a team inside a team.** When you ask a teammate to start its own nested team, there's now a clear in-app guide covering where the child team should live (its own folder, never the parent's), how the parent and child talk to each other, and an important safety rule: a child team must never shut down the folder the main team is running in.

## 0.3.22

- **More reliable message delivery, especially when something goes wrong with one message.** Previously, a single problem message — one whose destination had gone away, or that couldn't be confirmed as delivered — could quietly hold up the whole batch of pending messages behind it, so other teammates' messages stopped arriving too. Now each message is handled on its own: one bad message is set aside and reported, and everything else keeps flowing. The system also double-checks that a message actually landed on the recipient's screen before calling it delivered, and if a teammate's window has moved, it finds the new location instead of sending into a dead one. The result is that normal team chatter keeps moving even when one delivery hits trouble.

## 0.3.21

- **Fixed: messages from a teammate could get stuck and never reach the lead.** Previously, if a teammate's message happened to contain certain common words, the system could mistake it for a "crossed-wires" session and hold the message back instead of delivering it. Now the system reliably tells apart a genuine session problem from ordinary chat, so normal messages no longer get blocked — and if a real session problem does happen, it now corrects itself automatically instead of staying stuck.

## 0.3.20

- **Fixed: closing, restarting, or adding/removing teammates could fail on teams opened a certain way.** If you connected to a team using a custom setup, these management commands used to error out and couldn't find the right window. They now work reliably no matter how the team was opened.
- **Fixed: restarting a team would sometimes hang, claim the team was gone, and quit on its own.** On those same custom setups, a restart could wrongly decide the team "no longer exists" and give up halfway, even though the team was running fine. Restart now finds the team correctly and no longer bails out by mistake.

## 0.3.19

- **Fixed: reconnecting to a team could fail and not find the lead's window.** In some cases, rejoining a team failed because it remembered the wrong place to connect. It now finds the right place automatically and remembers it, so reconnecting works on the first try.
- **Improved: a misplaced startup option now gives you a clear message.** If you put a team's startup option in the wrong spot, it used to be silently ignored, leaving you guessing. Now it tells you right in the terminal exactly what's wrong instead of quietly dropping it.

## 0.3.18

- **Fresh teams created with `team-agent quick-start` are now correctly managed-topology by default again** (0.3.17 regression fix): the `is_external_leader` topology marker is now written explicitly on team creation (`false` for `quick-start`, `true` only for `--external-leader` opt-in or external attach), and every consumer (status, lifecycle, shutdown) reads it through a single helper. A missing marker is consistently interpreted as the managed default everywhere, so 0.3.17 teams could mis-classify themselves and `team-agent status` could falsely show ready when the managed leader receiver hadn't attached yet — both fixed. Managed teams now report a `degraded` / not-attached status when the leader receiver is missing, instead of silently saying ready.
- **`team-agent reset-agent --discard-session` actually starts a fresh worker context now**: previously a discard could leave a stale provider session id behind because the save path backfilled it from an in-flight capture. The save path is now tombstone-aware (it does not backfill a session after a discard), the next launch reports `start_mode = Fresh` with a new `session_id`, and the JSON output now carries structured `start_mode` / `discarded` / `new_session_id` so operators can verify the reset actually took.

## 0.3.17

- **`team-agent attach-leader` finds the leader pane on the first try in multi-team workspaces**: `--team` is now threaded through the lifecycle command port into the attach path, the attach logic selects team-scoped state and probes the tmux endpoint recorded in `leader_receiver.tmux_socket` / `tmux_socket` before falling back to the workspace default socket, and the endpoint that actually produced the pane is persisted back so subsequent attaches go straight to it. When the leader pane cannot be located, the error message now lists every endpoint that was searched instead of failing opaquely.
- **`--external-leader` opt-out is now fully wired** for `team-agent codex` / `claude` / `copilot`: the flag is recognised at the managed-launcher entry, persisted on the team as `is_external_leader = true`, suppresses the managed `:leader` window creation, and is honored across dispatch / restart / status. CLI usage is clear about putting `--external-leader` before any `--` provider passthrough separator, and contracts now cover the no-tmux / real-shape paths.

## 0.3.16

- **`team-agent <provider>` is now structurally unkillable from your terminal**: managed launchers (`team-agent codex`, `team-agent claude`, `team-agent copilot`) now create the leader provider process **inside** the team's tmux session and attach your terminal to it as a tmux client. Closing the terminal — or even `kill`-ing it from outside — only detaches the client; the leader provider and the rest of the team keep running. Same semantic as `tmux new-session -t ...` but with no manual tmux invocation. Status output, the team-agent skill, and `attach-leader` output all carry the `:leader` window pointer so re-attaching from anywhere is one command.
- **`--external-leader` opt-out keeps the old topology** for users who already drive the leader from their own terminal stack: pass `--external-leader` to `team-agent codex` / `claude` / `copilot` and the launcher behaves like before — the leader provider stays on the terminal you started it from, and the framework only treats it as the leader (no managed-session take-over). `state.is_external_leader` carries the choice forward through dispatch / restart / status; nested teams keep separate is_external_leader values so a parent and child team can each use the topology that fits.
- **`team-agent shutdown` no longer reports a false `partial`** when the coordinator hits a "late success" race (it finished cleanup just after the shutdown timer started reporting). The shutdown result classifier now folds the late-success outcome into the success bucket — the JSON / `lifecycle.shutdown` event now say `ok` when the coordinator did in fact stop cleanly.

## 0.3.15

- **Adaptive layout is on by default**: `team-agent quick-start` now lays out worker windows automatically — up to 3 panes per window (332 split for 8 workers becomes 3+3+2), agents added or restarted later land in the same adaptive layout instead of arriving as orphan windows, and the leader pane is never touched. The whole team shares one tmux socket and one window tree, so opening a second team in the same workspace doesn't fight the first.
- **`report_result` is now delivered to the leader even when the worker's MCP channel is down**: a worker that finishes after losing its MCP session no longer leaves the leader staring at an apparently-stuck worker. The runtime now records the final result through a leader-side fallback delivery path (`team-agent collect` continues to be the authoritative state-update path) and surfaces it with two new diagnostic subcommands and a single noisy audit event so missing report_results stop being silent. `team-agent diagnose` will point at the fallback record when MCP delivery was lost.
- **Verified RS recovery runbook is now a Team Agent skill reference**: `skills/team-agent/references/recovery-runbook.md` documents the supported recovery moves (Transport closed, leader pane re-attach, coordinator restart, etc.) for both human operators and Team Agent leaders. The skill description points at it so the runbook gets pulled in automatically when symptoms match.
- **`team-agent add-agent` no longer dead-locks or leaves a ghost role**: the runtime-state path is now the single source of truth for add/start-agent (no second-source write that could go stale), and a failure mid-add cleanly rolls back instead of leaving an agent record without a launched session.
- **`team-agent restart --allow-fresh` is no longer sticky across restarts**: a fresh session id is now persisted as the new expected session id, so the next restart resumes against the new session instead of asking for `--allow-fresh` again every cycle.

## 0.3.14

- **Copilot leader trust prompt is now actually auto-answered** in `team-agent copilot` interactive sessions: the trust-prompt handler that already existed in the worker tick is now also wired into the leader passthrough path on the current `TMUX_PANE`, so a fresh `team-agent copilot` boot no longer parks on `Confirm folder trust / Do you trust the files in this folder?` — it advances to ready on its own (0.3.13 changelog claim is now actually delivered).
- **`team-agent shutdown` JSON / event result is honest about what got killed**: bare shutdown now performs the shared-socket tail cleanup BEFORE computing the report, and the report includes `killed_sessions` / `spared_sessions` for the shared socket. Previously the JSON/event was computed before the cleanup step ran, so `ok: false, status: partial` could be reported even after the socket had been fully cleaned (0.3.13 changelog claim now actually delivered for the bare path).
- **Last remaining owner-attribution corner closed**: the seventh seed point (`lifecycle/launch.rs:3704-3758 seed_launched_owner_from_env`) previously defaulted to `codex` when `TMUX_PANE` was set but no explicit `TEAM_AGENT_LEADER_PROVIDER` env was passed, masking the 0.3.13 E22 fallback. Codex default removed; the seed now uses only the explicit env or attributes the caller pane via the existing `leader::attribute_pane_provider()` (and leaves owner/receiver unset rather than guessing). A `team-agent copilot` leader started from a tmux pane now writes `team_owner.provider = copilot` end-to-end.
- **`team-agent restart` no longer brings up a zombie session under a refused-resume worker**: the restart entry path now treats refused-resume as a single-worker failure and isolates it from the rest of the team (G1 regression for the resume-atomicity contract); honest exit codes and per-worker failure events keep working alongside it.

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
