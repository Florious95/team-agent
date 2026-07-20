# Team Agent Design Rules

This index records the architecture and governance rules enforced by
[`scripts/gate.sh`](scripts/gate.sh). Run the complete suite from the repository
root:

```bash
scripts/gate.sh
```

The command reports every item as `PASS` or `FAIL`, continues after individual
failures, and exits nonzero if any item fails. Its default Cargo target is
worktree-specific so compiled contracts cannot retain another worktree's
`CARGO_MANIFEST_DIR`; callers may still provide an isolated `CARGO_TARGET_DIR`.

## Module index

| Module or surface | Rule |
| --- | --- |
| `cli/send` | [C1: send dependency direction](#c1-send-dependency-direction) |
| `cli/status_port` | [C2: status read-model direction](#c2-status-read-model-direction) |
| `lifecycle/launch` | [C3: launch dependency direction](#c3-launch-dependency-direction) |
| `state` | [State-write authority](#state-write-authority) |
| Runtime `state.json` reads | [Raw-read surface](#raw-read-surface) |
| Split source surfaces | [Composite source guards](#composite-source-guards) |
| Production Rust files | [Line-count ledger](#line-count-ledger) |
| Contract tests | [R6 hermetic static guard](#r6-hermetic-static-guard) |

## C1: send dependency direction

- **Why:** resolving, persisting, and presenting a send must remain a one-way
  funnel; a presenter that resolves or persists reintroduces orchestration and
  makes delivery truth diverge.
- **Boundary:** `cli/send/resolve.rs -> persist.rs -> presentation.rs`.
- **Positive example:** `resolve.rs` calls `persist_resolved_target`, and
  `persist.rs` calls `delivery_outcome_json` after persistence.
- **Negative example:** `presentation.rs` imports `resolve` or `persist`, or a
  wildcard import hides that reverse edge.
- **Probe:** `scripts/gate.sh` item `reverse-edge-c1-send`.

## C2: status read-model direction

- **Why:** formatting must consume prepared values, not query the message store;
  otherwise data access can move ahead of validation and change error priority.
- **Boundary:** the `status_port` facade coordinates snapshot/store/format;
  `snapshot` may consume `store`, while `format` only formats caller-provided
  values.
- **Positive example:** `snapshot.rs` imports store readers and passes their
  results toward formatting.
- **Negative example:** `format.rs` imports `MessageStore`, `RuntimeSnapshot`, or
  `snapshot`; `store.rs` assembles a `RuntimeSnapshot`.
- **Probe:** `scripts/gate.sh` item `reverse-edge-c2-status`.

## C3: launch dependency direction

- **Why:** broad imports hide cycles between approval, spec-state, and state
  projection, making mechanical moves capable of silently restoring reverse
  dependencies.
- **Boundary:** `spec_state -> approval` and
  `state_projection -> spec_state`; the facade uses explicit re-exports.
- **Positive example:** `spec_state.rs` imports only the approval detector it
  needs, and `state_projection.rs` imports named spec-state helpers.
- **Negative example:** `approval.rs` imports `spec_state`, or `launch.rs`
  wildcard-re-exports any of these modules.
- **Probe:** `scripts/gate.sh` item `reverse-edge-c3-launch`.

## State-write authority

- **Why:** direct runtime-state saves outside the repository authority create
  competing write semantics and bypass typed reapply routing.
- **Boundary:** the frozen 29-row save-call snapshot may only shrink; the direct
  save family may not be aliased or imported outside `state/persist.rs`,
  `state/projection.rs`, and `state/repository.rs`.
- **Positive example:** a state transition routes through `StateRepository`, and
  removing an obsolete allowlist row lowers the baseline in the same change.
- **Negative example:** raising the baseline, resurrecting a deleted row, or
  importing `save_runtime_state as renamed_save` elsewhere.
- **Probe:** `scripts/gate.sh` item `state-write-ratchet`, which invokes the
  verifier-frozen HARD1 ratchet and HARD2 alias/import contracts.

## Raw-read surface

- **Why:** unclassified reads of canonical `state.json` bypass repository
  migration and routing semantics.
- **Boundary:** every production file mentioning `state.json` is enumerated by
  the frozen HARD3 scanner; the single optional, non-migrating ingress remains
  `StateRepository::load_workspace_if_exists_without_migrations`.
- **Positive example:** an optional diagnostic read calls the named repository
  facade.
- **Negative example:** a new production module reads or names `state.json`
  without verifier classification.
- **Probe:** `scripts/gate.sh` item `raw-read-scanner`.

## Composite source guards

- **Why:** source guards must keep seeing their intended semantic surface after
  a large file is mechanically split; moving a marker into a sibling module
  must not evade a contract.
- **Boundary:** signed send, status, and launch contracts read the facade plus
  its sibling module directory in deterministic relative-path order.
- **Positive example:** the E7 send guard finds registry markers in
  `cli/send/resolve.rs` even though they no longer live in `send.rs`.
- **Negative example:** a guard reads only `send.rs`, so deleting a required
  marker from all sibling modules still passes.
- **Probe:** `scripts/gate.sh` item `composite-source-guards`, which runs the
  existing E7, compact-status, and runtime-approval contracts without copying
  their assertions into the script.

## Line-count ledger

- **Why:** the repository still has historical large-file debt, so the honest
  gate is monotonic: debt cannot grow while already-split modules stay small.
- **Boundary:** at release `0.5.51`, at most 86 production Rust files may exceed
  500 lines, `temporary_debt` must stay empty, and every file in the split send,
  status, and launch surfaces must remain at or below 500 lines.
- **Positive example:** splitting another over-limit file lowers 86 to 85.
- **Negative example:** adding a new 501-line production file, or growing a
  split target back above 500 lines.
- **Probe:** `scripts/gate.sh` item `line-count-ledger`, backed by
  `tools/check_line_count_gate.py` plus the frozen monotonic ceiling.

## R6 hermetic static guard

- **Why:** a contract that touches processes, tmux, sockets, or environment
  state without an isolation boundary can pass locally while damaging a live
  team runtime.
- **Boundary:** dangerous test behavior must declare the established hermetic
  boundary before a contract is accepted.
- **Positive example:** a process-spawning contract uses the repository's
  hermetic test fixture/marker.
- **Negative example:** a test directly launches tmux or a coordinator without
  the hermetic boundary.
- **Probe:** `scripts/gate.sh` item `r6-hermetic-static-guard`, which invokes
  `r6_static_guard_rejects_dangerous_tests_without_hermetic_boundary` exactly.
