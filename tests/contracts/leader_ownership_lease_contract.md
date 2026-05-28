# Leader Ownership Lease Contract

This contract defines Gap 39 leader ownership and receiver binding behavior for Slice 0.2.4-1. It is based on the real S0 fixtures under `tests/fixtures/leader_ownership_lease/` and the constitution-reviewer C1-C23 verdict. Implementers may use this document as the black-box contract; the acceptance test source is owned by test-engineer.

## Model

Leader ownership is a lease. The persistent identity is the deterministic `leader_session_uuid`; pane ids are routing hints that can die or be rebound. A leader lease mutation means any acquire, claim, rebind, takeover, attach, or owner epoch advance that changes the effective owner or receiver.

Every lease decision is made from live evidence at decision time. Cached state fields can be input context, but never the final liveness source.

## Required Behavior

C1. Liveness is derived from live OS/tmux probes at decision time: pane existence, owning process existence, process tree, and process cwd. Cached `state.json` values such as `coordinator.pid` are not authoritative.

C2. A leader-shaped pane is detected by the process tree, not only by the immediate foreground command. If `claude`, `claude.exe`, `codex`, or the provider leader process appears in the ancestry, the pane is live leader-shaped even while a child command is foreground.

C3. Vacancy is revalidated while the runtime lease lock is held. The final bind is a compare-and-swap on `owner_epoch`; if liveness or epoch changed since precheck, the operation aborts and re-evaluates or refuses without stealing.

C4. No-confirm auto-acquire is allowed only for unambiguous death: the recorded pane is absent from tmux or the owning PID is gone. Ambiguous signals, including wrong foreground command or cwd mismatch while the pane/PID still exist, require `--confirm`.

C5. Acquire-on-vacant binds the caller pane only when all are true: caller is leader-shaped, same host, same OS user, and caller cwd is inside the workspace. Otherwise the mutator proceeds without claiming, queues safely, and emits `rebind_required`. A plain shell, cron, CI process, empty `$TMUX_PANE`, or worker pane never becomes leader receiver.

C6. The trust boundary is a single OS user. Same-user spoofing of a leader-shaped process is out of scope; different OS users are never eligible candidates.

C7. Workspace cwd comparisons realpath both sides before comparison, including symlinked workspaces and worktrees.

C8. Workspace membership is subtree containment: cwd equal to the workspace root or any descendant counts as inside the workspace.

C9. Cwd enumeration is only a superset filter. When multiple teams share one workspace, explicit `--team` or a human claim resolves the team. The runtime must over-include and broadcast rather than silently bind the wrong team.

C10. Injected `TEAM_AGENT_LEADER_SESSION_UUID` remains a fast path. If present and uniquely matching one live pane, it takes precedence and avoids broadcast. Without it, cwd plus leader-shape enumeration is used.

C11. `claim-leader`, `takeover`, and `attach-leader` converge on one lease-claim code path and therefore share identical safety gates.

C12. The claim path derives caller identity from the tmux pane the same way `identity` does. A valid pane does not require manual `TEAM_AGENT_LEADER_*` exports and does not fail with `no_caller_identity`.

C13. Claim semantics are: vacant owner binds without `--confirm`; live other owner refuses unless `--confirm`; current holder is an idempotent success and does not bump `owner_epoch`.

C14. The fallback claim command is operator-facing. Workers are not required to learn a new CLI path, and worker-to-leader delivery continues through existing message transport.

C15. Single-candidate acquire and multi-candidate broadcast claim share the same atomic `owner_epoch` compare-and-swap. First winner commits; losers receive `owner_epoch_advanced`.

C16. Broadcast occurs only when two or more live candidates remain after filtering. A single candidate or an env-unique match resolves silently.

C17. Every lease mutation writes `team_owner` and `leader_receiver` to both state locations in one runtime lock hold: workspace-level state and team-level state. Workspace-only or team-only writes are prohibited.

C18. After any mutation, both state files must agree on `owner_uuid`, `receiver_pane_id`, and `owner_epoch`. Divergence is a detectable audited error. Existing workspace `team_owner` and team-level owner naming must represent the same fact.

C19. Doctor/repair detects and heals owner/receiver divergence and stale `leader_receiver` panes.

C20. Every acquire-on-vacant, rebind, and epoch advance emits a structured audit event. Silent self-bind is prohibited. Required event types are `owner.adopted_on_restart`, `leader_receiver.rebind_applied`, and `owner_epoch_advanced` or semantically equivalent names with the required fields.

C21. Every refusal emits a structured audit event. Refusals include owner-gate refusal, lost epoch race, caller not leader-shaped, cwd mismatch, different user, and not-in-tmux-pane.

C22. Lease audit events carry a closed-enum `reason`, redacted uuid prefix, old pane id, new pane id, host, and OS user when known. The reason enum is:
`vacant_acquired`, `previous_owner_pane_dead`, `previous_owner_alive_refused`, `owner_epoch_advanced`, `force_confirm_required`, `caller_pane_missing`, `caller_cwd_mismatch`, `not_in_tmux_pane`.

C23. `coordinator.pid` false-negative status is a separate gap. This slice may read coordinator state for diagnostics, but leader lease liveness must not depend on cached coordinator pid state.

## Acceptance Surface

The acceptance tests cover the original Gap 39 cheap gates plus T8-T12:

1. A real stale receiver fixture where state points to dead pane `%648` can be acquired by the current valid caller pane without `--confirm`, and both state files are repaired.
2. A live recorded owner is not stolen without `--confirm`.
3. `claim-leader`, `takeover`, and `attach-leader` converge on the same lease mutation semantics instead of writing divergent state.
4. Caller identity is derived from the tmux pane and does not require manual `TEAM_AGENT_LEADER_*` environment export.
5. Cwd/team enumeration uses realpath subtree matching and explicit team resolution.
6. Two live candidate panes broadcast once and do not silently bind either pane.
7. Refusals and successful mutations emit auditable structured events with closed-enum reasons.
8. Busy or cd'd live leaders are not false-positive-dead and cannot be stolen.
9. TOCTOU owner death/revival races are resolved by epoch CAS.
10. Dual-state mutations are atomic and partial-write divergence is detected or repaired.
11. Non-leader callers, including plain shells outside tmux, never self-bind as leader receiver.
12. Symlinked workspace paths are compared by realpath and still match the canonical workspace.

The S0 fixtures intentionally preserve the broken 2026-05-27 substrate: `takeover` returned `claimed` while only workspace `team_owner` changed, `leader_receiver` remained the dead `%648`, `attach-leader` refused with `leader_uuid_missing`, and `claim-leader` refused with `no_ambiguous_candidates`. Those outputs are evidence for the tests; they are not acceptable post-fix behavior.
