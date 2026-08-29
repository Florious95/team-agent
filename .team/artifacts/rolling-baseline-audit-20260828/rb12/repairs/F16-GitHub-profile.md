# RB12 F16 GitHub scoped profile

## Disposition

Apparatus-only profile added to PR #58's `fix/combined-gates-safe-full-ci`
branch. This is ready for fresh independent read-only static review; it does
not authorize dispatch or claim F16 runtime execution.

## Frozen identity and scope

- Repository remote: `origin` (`git@github.com:Florious95/team-agent.git`).
- Branch/worktree: `fix/combined-gates-safe-full-ci` at
  `/private/tmp/pr-cg7-safe-full-ci`.
- Frozen pre-edit head: `e8768cbbbada10332cdf5162bfcf780a452cf988`.
- Pre-edit tree: `fea0a9321d3187d11fe8ad37ef21aad70689a6c4`.
- Profile implementation commit: `5a9bf64a0f44b4c6cd6af7cc5c94330a2786a164`.
- Implementation tree: `7c151086a511bb31683c94d09d7608460dbd8144`.
- `verify-full.yml` blob SHA: `b9c67915f271db73f0ecc25d2657940b3579a19e`.
- Final branch head/tree after this receipt commit and clean-status check are
  reported in the delivery result; the profile implementation identity above
  isolates the workflow change from this documentation commit.
- Pre-edit local head equaled `origin/fix/combined-gates-safe-full-ci` and the
  worktree was clean. After the normal non-force push, final local and origin
  heads equal the implementation commit before this receipt commit.
- Changed product/apparatus path: `.github/workflows/verify-full.yml` only in
  the implementation commit; this receipt is the sole additional artifact.

The existing `full`, `f04-r4`, `f05-rec002`, and `f14-containment` profiles are
unchanged. The new dispatch allowlist entry is exactly `f16-wleak`.

## Closed profile contract

The profile runs under the existing `set -euo pipefail` shell and executes
exactly once:

```text
cargo test -p team-agent --locked --test e2e cases::wleak_worker_delivery_socket_leak_contract::wleak_cached_pane_owned_by_other_window_never_receives_worker_message -- --exact --nocapture --test-threads=1
```

The existing exact-target checkout verification, Rust/Cargo `1.95.0`,
Ubuntu/macOS matrix, contents-read permissions, and raw GitHub step logs are
preserved. The workflow has no retry, timeout inflation, publish permission,
or profile-specific process/resource fallback. Unknown profile values remain
closed by the existing `*)` branch, which emits `Invalid test profile` and
exits non-zero. No workflow was dispatched and no Cargo/product test was run
for this apparatus change.

## Static validation

- Ruby Psych YAML parse: pass (workflow loaded as a mapping).
- Closed-profile assertion: pass; `f16-wleak` is allowlisted, the exact command
  is present once, and the profile contains exactly one Cargo invocation.
- Unknown-profile rejection assertion: pass; existing `*)` branch and
  `Invalid test profile: ${TEST_PROFILE}` remain present.
- `git diff --check`: pass.
- Cargo/product tests: deliberately not run (apparatus-only scope).

## Diff and delivery

The implementation commit changes four lines in one workflow: one dispatch
choice and one one-command case branch. The receipt artifact is added in this
follow-up commit. The final branch head/tree, workflow blob, remote equality,
and clean worktree were checked after the normal non-force push and are
reported with the completion result.

No PR #95/F16 source, candidate, target branch, release, publish operation, or
secret was accessed or changed.
