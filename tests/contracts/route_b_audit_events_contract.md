# Route B Auditability Contract

Route B restart semantics separate interacted workers from never-interacted workers. This contract defines the user-visible audit surfaces required around that decision model.

## C1: First Interaction Event

When the leader first successfully sends work to a worker and the runtime records that worker's first interaction timestamp, the event log must receive exactly one `worker.first_interaction` event.

Required event fields:

- `worker_id`: worker that received the first successful leader send.
- `first_send_at`: the exact ISO 8601 UTC timestamp persisted for that worker.
- `message_id`: the message that caused the first interaction stamp.

This event is idempotent per worker. Re-delivery of the same message or later leader sends to the same worker must not emit another `worker.first_interaction` event. Worker-to-worker peer sends must not emit this event, because they do not represent leader interaction with that worker.

## C2: Restart Resume Decision Event

Every restart attempt must emit one `restart.resume_decision` event for each non-paused worker considered by restart.

Required event fields:

- `worker_id`
- `has_first_send_at`: boolean
- `has_session_id`: boolean
- `allow_fresh`: boolean
- `decision`: one of `resume`, `refuse`, or `fresh_start`
- `first_send_at`: ISO 8601 UTC timestamp or null
- `session_id`: persisted session id or null

The decision values follow the Route B matrix:

- `resume`: the worker has a resumable persisted session.
- `refuse`: the worker has a first interaction timestamp but cannot resume, and restart was not run with `--allow-fresh`.
- `fresh_start`: the worker has no first interaction timestamp and cannot resume, or `--allow-fresh` explicitly permits a fresh start.

If any worker decision is `refuse`, restart must still emit the existing `restart.atomic_refusal` event as a separate event listing the refused workers.

## C3: Status Interaction Visibility

`team-agent status --json` must include an `interacted` field for every worker entry:

- ISO 8601 UTC timestamp when the worker has a first interaction timestamp.
- The literal string `never` when the worker has no first interaction timestamp.

`team-agent status --summary` keeps the existing five-line triage shape. To preserve existing zero-interaction output contracts, the agents line appends ` (N interacted, M never)` only when `N > 0`. When no workers have interacted, the agents line stays unchanged.

## C4: Atomic Refusal Evidence

When restart refuses because an interacted worker cannot resume, the returned error envelope and the `restart.atomic_refusal` event must expose the evidence for each refused worker.

The refused worker list must include each worker's `first_send_at` timestamp. The human-readable error message must also include the refused worker id, that worker's timestamp, and the `--allow-fresh` action hint so the operator can decide whether losing that interaction history is acceptable.

## Strict `first_send_at` Values

`first_send_at` accepts only:

- null or missing, meaning the leader has never successfully sent work to that worker.
- A valid ISO 8601 UTC timestamp string, meaning the leader has interacted with that worker.

Other values are corrupt state. Examples include an empty string, numeric zero, boolean false, or the literal string `null`. Restart must not classify corrupt values through loose truthiness. It must fail deterministically before fresh-starting or atomic-refusing that worker, emit `restart.first_send_at_invalid`, and return an error envelope with reason `invalid_first_send_at`.

The invalid event must include:

- `worker_id`
- `raw_first_send_at`
- `raw_first_send_at_type`
