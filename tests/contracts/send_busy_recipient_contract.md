# Send Busy Recipient Contract

Gap 42 fixes a false negative in tmux-backed message delivery. Delivery success
is determined by whether the runtime pasted the message into the target pane and
submitted it. A target pane being busy, compacting, or otherwise not yet showing
a new prompt marker is not a delivery failure.

## Successful Submit Is Authoritative

Once paste succeeds and the final submit attempt reports `submitted: true`, the
delivery envelope must be successful:

- `ok: true`
- `stage: submitted`
- `submitted: true`
- `submit_attempts[-1].submitted: true`

This remains true when no new prompt/turn marker is observed in the short
post-submit observation window.

## Busy Recipient Observation

When paste and submit succeed but no target prompt marker is observed because
the recipient is busy or compacting, the runtime returns:

- `turn_verification: not_yet_observed`

It must not return:

- `ok: false`
- `status: failed`
- `stage: turn-boundary-verification`
- `turn_verification: leader_new_turn_boundary_missing`

No `send.failed` event is emitted for this condition. A successful send path
emits the normal submitted event with `turn_verification: not_yet_observed`.

## Idle Recipient Observation

When a prompt/turn marker is observed after submit, the runtime still returns
success with:

- `turn_verification: leader_new_turn_boundary_verified`

## Real Failures

Paste or submit failures are still failures. The contract only demotes the
post-submit turn-boundary probe from a hard gate to observation metadata.

## Scope

The contract applies to the common tmux injection envelope used by leader to
worker, worker to worker, and worker to leader delivery paths. Higher-level
`send_message` results must propagate the same distinction: submitted but not
yet observed is successful delivery, while paste/submit failure remains failed.
