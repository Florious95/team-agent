# Trust Truncated Workspace Contract

## Scope

This contract governs Team Agent's opt-in Codex trust-prompt auto-answer behavior when the provider pane displays a shortened workspace path.

The runtime's known worker workspace cwd is the source of truth. The path shown in the Codex trust prompt is a consistency guard only. A shortened display path must not be treated as a literal different workspace when it is consistent with the runtime cwd.

Hard truncation is only trustworthy when the capture proves the path token reached the pane's right edge. A plain string-prefix match is not enough: `/repo` and `/repo-backup` share a prefix but are different sibling directories.

## Required Behavior

1. Auto-answer remains opt-in. Without an explicit auto-trust opt-in, the trust prompt is not answered.
2. When auto-trust is opted in and the prompt path exactly equals the runtime workspace cwd after normal path canonicalization, Team Agent answers the trust prompt.
3. When auto-trust is opted in and the prompt path is hard-tail-truncated by the terminal, Team Agent answers only if both conditions hold:
   - the canonical runtime workspace string starts with the canonical captured prompt path;
   - the captured path token reaches the capture line's right boundary, using the pane width captured with the fixture or equivalent pane metadata.
4. If the captured path token does not reach the right boundary, prefix matching is forbidden. The captured path must be treated as a complete path token and must match the runtime workspace exactly.
5. When auto-trust is opted in and the prompt path contains a middle ellipsis (`...` or the Unicode ellipsis `…`), Team Agent answers if the text before the ellipsis is a prefix of the canonical runtime workspace and the text after the ellipsis is a suffix of the canonical runtime workspace.
6. When auto-trust is opted in and the prompt path is a boundary-safe ancestor directory of the runtime workspace, Team Agent answers only when that ancestor token reaches the capture line's right boundary. This is treated as a valid hard-truncation/viewport-consistency case because the runtime cwd remains the source of truth.
7. Team Agent must still refuse `workspace_dir_mismatch` when the captured prompt path is a genuinely different directory: not equal to the runtime workspace, not a right-edge truncation of it, not an ellipsis match, and not a right-edge ancestor truncation.
8. Shared-prefix sibling directories must be refused when the captured token is complete. Both directions are mismatches: runtime `/repo-backup` with captured `/repo`, and runtime `/repo` with captured `/repo-backup`.
9. The existing refusal reason `workspace_dir_mismatch` is preserved for genuine mismatches.

The public call surface must carry enough pane metadata to distinguish right-edge truncation from a complete shorter token. For the current public API, `attempt_trust_auto_answer(...)` receives this as `state["pane_width"]`, an integer pane width in display columns for the capture.

## Live Delivery Wiring

The live delivery path is part of this contract, not an optional caller detail.

When `_deliver_pending_message(...)` receives a `codex_trust_prompt` non-input envelope from the tmux injection path, it must query the target tmux pane width at that moment and pass that width to the trust matcher before deciding whether the workspace matches. The existing `state["pane_width"]` field is only the carrier into `attempt_trust_auto_answer(...)`; production behavior may not rely on that field already being populated by unrelated startup code.

The wiring acceptance seam is:

```text
team_agent.messaging.delivery._tmux_pane_width(target) -> {"ok": true, "pane_width": <int>}
```

or, on failure:

```text
team_agent.messaging.delivery._tmux_pane_width(target) -> {"ok": false, "error": "..."}
```

For a trust prompt detected during live message delivery:

1. Delivery queries `_tmux_pane_width(target)` for the same pane that produced the trust prompt.
2. If the query succeeds, delivery passes that integer as `state["pane_width"]` to `attempt_trust_auto_answer(...)`.
3. If the query fails or returns no positive integer, delivery must fail safe. It must not auto-answer a hard-truncated prefix based on `pane_width=None`.
4. A right-edge truncated workspace that matches the runtime cwd must not emit `leader_panes.trust_auto_answer_refused` with `reason="workspace_dir_mismatch"` merely because the original runtime state lacked `pane_width`.

## Real Fixture

The blocker fixture is the verbatim Codex pane capture where the displayed path is hard-truncated without an ellipsis and reaches the pane's right edge. Its pane width fixture is `80`.

```text
> You are in /Users/alauda/team-agent-test/workspaces/0.2.4-bundled-20260528T014

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue
```

The runtime-held workspace cwd for that pane was:

```text
/Users/alauda/team-agent-test/workspaces/0.2.4-bundled-20260528T014841Z-gap39
```

That pair must be accepted when auto-trust is opted in.

## Residual Risk

Terminal hard truncation loses information. Team Agent accepts the right-edge prefix case only because the worker was launched by the runtime with `cwd` equal to the full workspace path and that full cwd is deterministically known outside the pane capture. If the runtime cannot prove the full cwd or cannot prove that the prompt token reached the pane right edge, it must not auto-answer by prefix alone.
