# RB12 F16 refreeze on accepted F14

Status: `refreeze-ready pending dual review` (Linux selector apparatus-unjudgeable).

## Provenance and scope

- PR/branch: #95, `portfolio/f16-wleak-terminal-routing`.
- Declared worktree path `/Volumes/nvme/tmp/team-agent-f16-wleak` was absent; the existing exact branch worktree was `/Volumes/nvme/tmp/team-agent-f16-wleak-terminal`.
- Old accepted F16 head: `e5da4ee3e08fd1a2fde538614d6659df7402a35e`; pre-merge worktree was clean and tracked the expected origin branch.
- Accepted F14 parent: `312d19a38330cf9389fa69f915709f6ec7477b1e`.
- Normal non-FF merge commit: `26354c2f20ccb01981c4a21693bc9763a6f55998` (parent 1 old F16, parent 2 F14).
- F14 is an ancestor of the merge head. `git diff 312d19a3..26354c2f` is exactly `crates/team-agent/tests/e2e/cases/wleak_worker_delivery_socket_leak_contract.rs` (20 insertions, 1 deletion). No F17 change.

## Required selector evidence

- macOS final head: `cargo test --locked --package team-agent --test e2e cases::wleak_worker_delivery_socket_leak_contract::wleak_cached_pane_owned_by_other_window_never_receives_worker_message -- --exact --nocapture --test-threads=1`; `running 1`, `1 passed`, `0 failed`, `0 ignored`, `67 filtered`, exit 0.
- Linux fresh unit `gb-26354c2f20cc-f16-wleak-a1`: preflight passed on Rust/Cargo 1.95, but sync/run did not establish a unit and status returned `State=missing`, `status_rc=4`, with no authoritative `ExecMainStatus`/`CommandExit`. Classified apparatus-unjudgeable; no retry-until-green. See `raw/F16-refreeze/linux-grok-missing.txt`.

## Causal physical-routing tooth

At the refrozen source, a temporary, uncommitted mutation made delivery choose the live cached foreign window-b pane before normal session/window resolution. The required selector then failed red: exit 101, `running 1`, `passed 0`, `failed 1`, `ignored 0`, `filtered 67`. The token was absent from intended pane-a and present in pane-b. Message-bound `message.delivered` and `turn_open.armed_after_delivery` both recorded `resolved_from=temporary_foreign_window_b`, session `team-wleak001`, window `b`, pane `%1`, PID `90941`, endpoint `/private/tmp/tmux-501/ta-24ada15aa7d7`. The stale-binding event recorded cached pane `%1`, expected window `a`, observed window `b`.

The temporary production mutation and temporary capture instrumentation were removed. `crates/team-agent/src/messaging/delivery.rs` now hashes to the accepted F14 blob `44999d2ec30e6d0987a792402653bd5b27a73bab`; the final selector was rerun green. Raw diff/hash and pane/event captures are under `raw/F16-refreeze/`.

## Gates

- `cargo check --locked --package team-agent`: exit 0 (pre-existing warnings only).
- `rustfmt --edition 2021 --check crates/team-agent/tests/e2e/cases/wleak_worker_delivery_socket_leak_contract.rs`: exit 0.
- `git diff --check`: exit 0.
- Full `cargo fmt --all -- --check`: exit 1 on pre-existing unrelated repository-wide formatting drift; not caused by this refreeze and retained as an explicit non-green apparatus result.
- Clean pinned architecture tooling: `/Users/alauda/team-agent-scratch/wiki-tooling.worktrees/wiki-m2-engine`, commit `f980d66a3d5428d1e91758dd9d321696b08e2eae`, `build_wiki.py` SHA256 `8be942128de1a695854ea01801e35343d5426be225eb917acd97fe3ac7f8a69a`, `code_abstract_tree.py` SHA256 `4d99c38b928e6cdbe3e195ebdf9ce80295c9e658f67e9de18cce8994024a2684`. Exact-file `code_abstract_tree.py --direction C` outputs match after normalizing only worktree path headers. The repository's main tooling checkout is dirty, so it was not used.

## Finalization

Final worktree is clean after the normal commit. Final branch head/tree, push equality, and clean worktree are recorded after push below.
