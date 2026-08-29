# RB12 F17 GitHub scoped profile

## Disposition

Apparatus-only closed profile added to existing PR #58 apparatus branch
`fix/combined-gates-safe-full-ci`. This is ready for fresh independent
read-only static review; no workflow dispatch or Cargo/product test execution
was performed.

## Frozen identity and scope

- Repository remote: `origin` (`git@github.com:Florious95/team-agent.git`).
- Branch/worktree: `fix/combined-gates-safe-full-ci` at
  `/private/tmp/pr-cg7-safe-full-ci`.
- Frozen pre-edit head/tree: `58e6b592ccf69a43779de6b830f9609cd723d7bc` /
  `92d7b67b08b40067826e4d2715eac85b48f1736c`.
- Frozen pre-edit `verify-full.yml` blob:
  `b9c67915f271db73f0ecc25d2657940b3579a19e`.
- Profile implementation commit/tree:
  `0005057a0086ad6e6322096f07e68fda10a4dc4d` /
  `e37b6f94942466c504ec96f48a7ce413f263da08`.
- Final workflow blob after implementation:
  `46ff941eb140995e7ba7291c1470d84f24ae0c3c`.

The pre-edit worktree and remote were clean/equal. The implementation commit
changes only `.github/workflows/verify-full.yml` (four insertions): one closed
dispatch choice and one one-command `case` arm. Existing `full`, `f04-r4`,
`f05-rec002`, `f14-containment`, and `f16-wleak` profile bytes are unchanged.

## Closed profile contract

The dispatch allowlist now contains exactly `f17-wleak-metadata`. Under the
existing `set -euo pipefail` shell it runs exactly once:

```text
cargo test -p team-agent --locked --test e2e cases::wleak_worker_delivery_socket_leak_contract::wleak_message_delivered_event_records_physical_target_metadata -- --exact --nocapture --test-threads=1
```

The existing exact target-SHA checkout proof, Rust/Cargo `1.95.0`,
Ubuntu/macOS matrix (`ubuntu-latest`, `macos-14`), `contents: read`
permissions, and no-publish boundary are preserved. Unknown profile values
still reach the existing `*)` branch, emit `Invalid test profile`, and exit
nonzero. No retry, timeout change, extra selector/full invocation, assertion
change, or publish permission was added.

## Apparatus validation

- Ruby Psych YAML parse: pass.
- Closed-profile static assertions: pass; allowlist count 1, exact command
  count 1, one Cargo invocation, and unknown-profile fail-closed branch
  present.
- `git diff --check`: pass.
- Cargo/product tests: deliberately not run under this apparatus-only task.
- Workflow dispatch/API calls: deliberately not made.
- No secret, PR #96/F17 source, candidate, target branch, release, or publish
  operation was accessed or changed.

## Delivery

The implementation commit was pushed by normal non-force push and the remote
ref equals `0005057a0086ad6e6322096f07e68fda10a4dc4d`. This receipt is a
separate documentation commit; final head/tree, workflow blob, remote
equality, and clean worktree are recorded in the completion result after its
normal push.

Fresh independent static review remains required before exactly one dispatch
at target SHA
`2b07695c70d146b5755eb1551bea7dbb48336dae`. This profile is apparatus-ready
only and does not claim runtime execution or acceptance.
