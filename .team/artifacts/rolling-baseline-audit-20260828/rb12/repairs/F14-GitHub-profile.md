# RB12 F14 GitHub scoped profile

## Disposition

Apparatus-only profile added to PR #58's `fix/combined-gates-safe-full-ci`
branch. This is ready for fresh independent read-only static review; it does
not authorize dispatch or claim F14 runtime execution.

## Frozen identity and scope

- Repository remote: `origin` (`git@github.com:Florious95/team-agent.git`).
- Branch/worktree: `fix/combined-gates-safe-full-ci` at
  `/private/tmp/pr-cg7-safe-full-ci`.
- Pre-edit local head: `371848b3cacf32e915be227b159bdd5f558591fe`.
- Profile implementation commit: `9ade07e3573f6481a191aa2bc69c8193f6612953`.
- Implementation tree: `0457034a2069696bb9cc07871d074d012cd3438f`.
- `verify-full.yml` blob SHA: `054edf7a6401943a279c3fac5200ef23537d3a94`.
- Pre-edit remote status: after fast-forwarding the remote's `371848b3` profile
  commit, local and `origin/fix/combined-gates-safe-full-ci` were equal and
  clean; no local edits were present before this change.
- Changed product/apparatus path: `.github/workflows/verify-full.yml` only.

The existing `full`, `f04-r4`, and `f05-rec002` profiles are unchanged. The
new dispatch allowlist entry is exactly `f14-containment`.

## Closed profile contract

The profile runs under the existing `set -euo pipefail` shell and executes
exactly these commands, sequentially and in this order:

```text
cargo test -p team-agent --locked --test e2e framework::containment_tests::f14_owned_socket_registration_is_exact_and_drop_spares_unregistered_socket -- --exact --nocapture --test-threads=1
cargo test -p team-agent --locked --test e2e cases::send_001_fake_worker::send_001_delivers_to_fake_worker -- --exact --nocapture --test-threads=1
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
- Closed-profile assertion: pass; `f14-containment` is allowlisted, both exact
  commands are present in the required order, and the profile contains exactly
  two Cargo invocations.
- Unknown-profile rejection assertion: pass; existing `*)` branch and
  `Invalid test profile: ${TEST_PROFILE}` remain present.
- `git diff --check`: pass.
- Cargo/product tests: deliberately not run (apparatus-only scope).

## Diff and delivery

The implementation commit changes five lines in one workflow: one dispatch
choice and one two-command case branch. The receipt artifact is added in this
follow-up commit. Before push, branch equality must be checked against
`origin/fix/combined-gates-safe-full-ci`; the final head/tree and clean status
are reported in the completion receipt after the normal non-force push.

No PR #87/F14 source, candidate, target branch, release, publish operation, or
secret was accessed or changed.
