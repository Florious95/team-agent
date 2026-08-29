# RB12 F15 refreeze on accepted F14

Status: **refreeze-ready pending dual review**.

## Frozen provenance

- PR: #93; branch: `portfolio/f15-gate061-documented-smoke`.
- Old accepted F15 head: `94dc34f80e7d625dfcbb3d5b1ff402733ab1296c`; pre-merge remote matched it.
- Accepted F14 parent: `312d19a38330cf9389fa69f915709f6ec7477b1e`.
- Normal merge commit: `879e5ad4a3f5de1ce1b4c29622d439800f21a45a` (parents old F15 and accepted F14).
- The merge was performed from a clean tracked worktree with `git merge --no-ff`; F14 is an ancestor.
- Direct F14-to-F15 source scope is exactly `crates/team-agent/tests/e2e/cases/gate_hole_061_red.rs`.
- `crates/team-agent/tests/e2e/framework.rs` is byte-identical to accepted F14. No F14 files were edited.
- Source tree at the tested merge head: `3744696257ba27a844f427dc303c9abcb8f175c8`.

## Owned selectors

1. `cases::gate_hole_061_red::tooth_1_existing_launch_smoke_runs_documented_quick_start_verbatim`
2. `cases::gate_hole_061_red::tooth_2_existing_send_smoke_proves_worker_receive_report_and_collect`

Both exact selectors passed independently on macOS with `--locked --exact --nocapture --test-threads=1` (1 passed, 0 failed, 0 ignored, 67 filtered each). After mutation restoration, both were rerun and passed with the same inventory.

Linux used exactly one fresh detached unit, `gb-879e5ad4-f15-gate061-a1`, sourced from a clean detached checkout at merge commit `879e5ad4a3f5de1ce1b4c29622d439800f21a45a`. The sequential fail-fast command ran both exact selectors. Fetch verification recorded `ExecMainStatus=0`, `CommandExit=0`, and 1 passed / 0 failed / 0 ignored / 67 filtered for each selector. Cgroup receipt was `gb-cgroup-receipt-v1`, `Policy=optional_degraded`, `Outcome=applied`; artifacts were verified, remote unit wiped with tombstone, and local scratch cleanup succeeded. Post-wipe status recorded `Wiped=yes`.

## Mutation teeth

- Tooth 1: changed only the documented argv canary from `.team/current` to `.team/not-current`; exact selector failed rc 101 at the intended argv assertion. Restored byte-exact.
- Tooth 2: changed only the result correlation from `truth.task_id == message_id` to `wrong-message-id`; exact selector failed rc 101 while waiting for same-message worker result/report evidence. Restored byte-exact.
- Restored owned-file SHA-256: `82bcc667baf1c9f29c04e2121cf57ceb0812ff0a0de84581a5f998d0cafa2099`, matching the clean tested source checkout. Mutation diffs, full logs, and digests are under `raw/F15-refreeze/mutations/`.

## Static and architecture gates

- `cargo check --locked -p team-agent --test e2e`: pass.
- `git diff --check 312d19a3..879e5ad4`: pass.
- `cargo fmt --all -- --check`: rc 1 due existing unrelated formatting outside F15; owned-file `rustfmt --check` reports the same inherited tooth3 formatting discrepancy. No unrelated formatting was changed.
- Clean content-hashed architecture tooling: detached `wiki-tooling` HEAD `e5bfb23aa4be2aa715503aab711078452a174d57`, tree `966eb9291016c08a5b0e838a1f7a3849affd00dc`, clean status. `build_wiki` on the enclosing 41-file E2E cases scope is explicitly `incomplete` (current unresolved 39; accepted-F14 unresolved 40), not green. Current and accepted-F14 abstract trees (A/B/C) are retained; their only semantic delta is the declared F15 architecture contract header. The single-file build scan was `empty` (0 files), so it is not used as acceptance evidence.

## Evidence index

Raw pre-merge, macOS, Linux lifecycle, mutation, static-check, and architecture evidence is under `rb12/repairs/raw/F15-refreeze/`. The Linux fetched receipt and artifact digest are retained under `raw/F15-refreeze/linux/evidence/`.

No product code, framework changes, timeout inflation, retries, sibling/tooth3 apparatus, F16/F17 files, candidate, target merge, tag, release, or publish was performed.
