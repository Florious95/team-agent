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

## Non-Requirements

This contract does not prescribe how the implementation avoids the stray turn. It may use a different trust-answer key sequence, wait for a Codex prompt state, inspect Codex turn records, detect queued-message UI, or another observable mechanism. The required external behavior is that auto-answer side effects are not reported as successful Team Agent delivery.
