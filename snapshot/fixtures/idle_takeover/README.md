# Idle Takeover Fixtures

Fixtures in this directory are inputs for the Gap 32 idle/takeover acceptance contract.

`*.real.jsonl` files are copied from local Codex rollout JSONL or Claude transcript JSONL samples recorded in `.team/artifacts/roundtable-idle-takeover-2026-05-27/turn-state-markers-evidence.md`.

`*.schema-derived.jsonl` files are not live-run captures. They are derived from the Codex source schemas cited by developer research because local archives did not contain real Codex failed-turn or permission-request records:
- `codex-rs/app-server-protocol/schema/json/v2/TurnStartedNotification.json` defines `TurnStatus = completed | interrupted | failed | inProgress`.
- `codex-rs/app-server-protocol/src/protocol/common.rs` defines app-server approval request methods including `item/commandExecution/requestApproval` and `item/permissions/requestApproval`.
- `codex-rs/hooks/src/events/permission_request.rs` defines hook `PermissionRequest` fields including `session_id`, `turn_id`, `tool_name`, and `tool_input`.

Mac mini E2E must later replace or supplement those schema-derived fixtures with real Codex captures.
