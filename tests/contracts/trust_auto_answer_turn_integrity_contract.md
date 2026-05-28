# Trust Auto-Answer Turn Integrity Contract

## Scope

This contract covers Codex trust-prompt auto-answer followed by live Team Agent message delivery.

Auto-answering the Codex trust prompt must be transparent to the worker's model-turn stream. The `1` used to select "Yes, continue" is a UI control action, not user work. It must not become a Codex user turn, and it must not make the next Team Agent brief fall into Codex's queued-message area while Team Agent reports a successful send.

## Fixture Evidence

Real incident: `0.2.4-bundled-20260528T033300Z`, candidate `integration/0.2.4 @ 95194cd760ccf4ab195f95d5c6924c8d838353e6`, result `res_95b40f1add79`.

Local fixtures copied from the latest Mac mini E2E artifact:

- `tests/fixtures/trust_auto_answer_turn_integrity/codex-worker1-gap39-fail.raw.txt`
- `tests/fixtures/trust_auto_answer_turn_integrity/codex-worker1-gap39-fail.ansi.txt`
- `tests/fixtures/trust_auto_answer_turn_integrity/gap39-dead-owner-restart.events.jsonl`

The raw pane fixture shows all three failure signals together:

```text
› 1

• Working (6m 02s • esc to interrupt)

• Messages to be submitted after next tool call (press esc to interrupt and send
  immediately)
  ↳ Team Agent message from leader:

    GAP39_PRIME_0.2.4-bundled-20260528T033300Z: reply via report_result summary
    …
```

The events fixture shows Team Agent emitted `leader_panes.trust_auto_answered`, then retried the same message, then emitted `send.submitted` with `turn_verification="leader_new_turn_boundary_verified"`. That was a false success: the Team Agent brief was only visible in Codex's queued-message area while Codex was busy on the stray `1` turn.

## Required Behavior

1. After Team Agent auto-answers a Codex trust prompt, the Codex model-turn sequence must not contain a user turn whose content is only `1`, nor any other visible model-turn artifact of the trust-choice key sequence.
2. The Team Agent brief sent immediately after trust auto-answer must become the next real Codex model turn's content. A message shown under "Messages to be submitted after next tool call" is queued, not delivered to the model.
3. Team Agent must not emit `send.submitted`, return `ok=true`, or stamp first interaction when the only evidence is that the message appears in Codex's queued-message area while Codex is already working on another turn.
4. A Codex prompt marker such as `› 1` followed by a queued-message block must not satisfy `leader_new_turn_boundary_verified` for the Team Agent brief. The active turn content must be the Team Agent brief itself.
5. If delivery cannot prove the Team Agent brief is the next real model turn after trust auto-answer, it must return a structured non-success result and leave the message eligible for retry or operator attention. It must not silently convert queued-message visibility into submitted success.
6. Prevention is required upstream of the recognizer. `attempt_trust_auto_answer(...)` must not send the `1` + Enter choice until the Codex trust prompt is actually ready for input. "Trust text is visible in scrollback" is not sufficient. The ready signal must be stable, for example a pane mode/input-ready signal or two consecutive equivalent snapshots showing the same trust prompt at an input-ready boundary.
7. If the trust prompt never becomes input-ready within the bounded wait, auto-answer must fail safe: no `1` key sequence is sent, no `leader_panes.trust_auto_answered` event is emitted, and the caller receives a structured non-success reason such as `trust_prompt_not_input_ready`.
8. After a trust prompt is dismissed and before retrying the original Team Agent brief, the live delivery path must confirm Codex is idle and ready for a new user turn. "The trust prompt disappeared" is not sufficient. If Codex is still working on any user turn, including a stray `1` turn, the Team Agent brief must not be pasted.
9. If Codex does not become idle before the bounded pre-brief gate expires, delivery must fail safe with a structured non-success result and no `send.submitted` event. The message may be retried or surfaced for operator attention, but it must not be reported as submitted.

## Non-Requirements

This contract does not prescribe the implementation technique. It may use a different trust-answer key sequence, wait for a Codex prompt state, inspect Codex turn records, detect queued-message UI, or another observable mechanism. The required external behavior is that auto-answer side effects are prevented before they create a model turn, and any residual queued-message state is not reported as successful Team Agent delivery.
