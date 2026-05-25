# Legacy DB Schema Compatibility Acceptance Plan

Purpose: verify a new Team Agent runtime can resume from artifacts left by the
previous runtime schema and still complete worker-to-leader communication.

## Required Fixture

- Workspace contains `.team/runtime/team.db` created with the legacy 14-column
  `messages` table, without `delivery_attempts`.
- The same DB contains at least one historical `messages` row, one pending
  `result_watchers` row, and one uncollected `results` row.
- Runtime state and spec look like a stopped or restartable Team left by the
  previous version.

## Automated Local Coverage

`tests/run_tests.py` includes a fixture that creates the legacy DB shape,
starts the new `MessageStore`, runs `coordinator_tick`, and verifies:

- `messages.delivery_attempts` is added by migration.
- New explicit-column writes work after migration.
- The legacy pending watcher is marked `notified`.
- The leader notification message is created and submitted.
- A running coordinator pid without new metadata is treated as incompatible and
  replaced by the new coordinator.

## Real Provider Acceptance

On Mac mini or equivalent real Team Agent host:

1. Install the candidate runtime.
2. Create a workspace from a previous-version trace: legacy `team.db`, runtime
   state, messages, result watcher, and result rows.
3. Restart the Team with the candidate runtime.
4. From the leader, send a prompt instructing the worker to send this exact
   message back to the leader: `好喽`.
5. Pass only if the leader receives `好喽` without manual DB repair, without
   manual `attach-leader`, and without a SQLite 14/15 column error.
6. Preserve command logs, `.team/logs/events.jsonl`, `team.db` schema after
   migration, leader/worker pane capture, and cleanup proof.

Fail fast on any product issue; rerun from a clean legacy fixture after fixes.
