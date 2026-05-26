# Restart Resume Atomicity Contract

This is the Route B behavior contract for restart. It is intentionally black-box: implementers should satisfy the behavior without depending on test internals.

## State Field

Each worker state record may contain `first_send_at`:

- Location: `state.json` under `agents.<id>.first_send_at`.
- Value: ISO 8601 UTC timestamp recorded by the framework when the leader first successfully sends work to that worker.
- `null` or missing means the leader has never successfully sent work to that worker.

## Decision Table

For each worker during restart:

| `first_send_at` | `session_id` | Without `--allow-fresh` | With `--allow-fresh` |
| --- | --- | --- | --- |
| present | present and resumable | Resume worker | Resume worker |
| present | missing or unresumable | Fail restart atomically and roll back newly created resources | Fresh-start that worker |
| null or missing | present and resumable | Resume worker | Resume worker |
| null or missing | missing or unresumable | Fresh-start that worker; this is not an atomicity violation | Fresh-start that worker |

## Atomicity Trigger

The only missing-session case that triggers atomic refusal is:

`first_send_at` is present, but the worker cannot be resumed because its persisted session is missing or unresumable.

Workers with no `first_send_at` have no accumulated conversation state. Fresh-starting them during restart is equivalent to their initial quick-start state and must not be reported as context loss.

## Required Outcomes

- If every worker with `first_send_at` can resume, restart succeeds even when never-interacted workers fresh-start.
- If any worker with `first_send_at` cannot resume and `--allow-fresh` was not passed, restart fails before committing partial new resources and reports the affected worker ids.
- If `--allow-fresh` is passed, workers with missing or unresumable sessions may fresh-start, including workers that have `first_send_at`.
- A successful restart that only fresh-starts never-interacted workers must not emit `restart.atomic_refusal`.
