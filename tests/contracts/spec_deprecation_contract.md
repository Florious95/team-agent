# Spec Deprecation Contract: `runtime.auto_trust_own_workspace`

This contract is for the public spec-loading boundary, not the trust-prompt auto-answer call path.

External API under contract:

```python
from team_agent.spec import load_spec

spec = load_spec(Path("team.spec.yaml"))
```

Required behavior:

1. When `runtime.auto_trust_own_workspace` is absent, `load_spec(path)` validates and returns the spec without writing a warning to stderr and without emitting `trust_auto_answer_spec_opt_in_deprecated`.
2. When `runtime.auto_trust_own_workspace` is `false`, `load_spec(path)` validates and returns the spec without writing a warning to stderr and without emitting `trust_auto_answer_spec_opt_in_deprecated`.
3. When `runtime.auto_trust_own_workspace` is `true`, `load_spec(path)` must warn immediately during spec load. The warning must be visible on stderr, mention `TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE`, and mention removal target `0.3.0`.
4. The same `true` load must emit a structured audit event named `trust_auto_answer_spec_opt_in_deprecated` to the workspace event log at `path.parent/.team/logs/events.jsonl`.
5. The structured event must include:
   - `deprecated_field="spec.runtime.auto_trust_own_workspace"`
   - `preferred_opt_in="env:TEAM_AGENT_AUTO_TRUST_OWN_WORKSPACE"`
   - `removal_target_version="0.3.0"`
6. In a single process, the stderr warning is one-shot. Loading the same deprecated spec three times emits the stderr warning once.
7. Audit events are per load, not one-shot. Loading the same deprecated spec three times emits three `trust_auto_answer_spec_opt_in_deprecated` events.

Rationale:

The deprecation is attached to the YAML spec field itself. A user who has this field in `team.spec.yaml` must be notified as soon as the spec is loaded, even if startup code handles the Codex trust prompt before the lazy trust-auto-answer path is called.
