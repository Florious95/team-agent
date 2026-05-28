# Trust Auto-Answer Minimal Delivery Contract

## Rationale

Round 1-4 is withdrawn. Those contracts restored the kind of busy/idle
classification that Gap 42 intentionally removed: after Team Agent has
successfully pasted text and submitted Enter, delivery must not infer failure
from what the provider pane happens to show.

The constitution-reviewer guideline update will codify this principle; see
section `§x.y` once the final numbering lands.

## Scope

This contract covers the Codex trust-prompt auto-answer action and the Team
Agent brief delivery immediately after the trust prompt is dismissed.

## Required Behavior

1. Trust auto-answer must submit the currently selected Codex trust choice with
   Enter only. It must not paste or type the choice digit `1`, or any other
   visible choice text, into the pane.
2. After trust auto-answer, if Team Agent pastes the brief and sends Enter
   successfully, delivery is successful regardless of the current visible Codex
   pane content.

## Non-Requirements

This contract does not require Team Agent to classify Codex as busy or idle,
inspect queued-message regions, identify unrelated active prompts, or narrow
submit verification based on provider UI text.
