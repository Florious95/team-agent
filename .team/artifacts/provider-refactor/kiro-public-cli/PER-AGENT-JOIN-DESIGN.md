# Per-agent join — implementation working note

Current source: c392d632; this is a design/trace note, NOT a candidate or native PASS. Leader msg_6fd7d38a2358 explicitly supersedes whole-team isolation; msg_68db44345af0 keeps all implementation in this single-owned worktree.

## Selected direction

One logical Team, route each recipient once. Kiro physical work stays in K3's store/outbox/Supervisor and K2 executor. Legacy recipients keep their existing public messaging/lifecycle path and provider internals. Do not register a fake legacy SeatRecord in K3, and do not give a private Kiro `%0` to an old default/shared-tmux transport.

- Legacy CLI/MCP → common messaging entry → registry lookup → Kiro framework ingress OR unchanged legacy path, before legacy persistence/injection.
- Kiro MCP remains the new bounded/fenced three-tool server. Same-store Kiro recipients use its existing outbox. Non-local/legacy/leader recipients use **durable forwarding intents**, not a second recipient physical queue.
- A framework forwarder delegates such an intent to existing `messaging::send_message` with a fixed `SendOptions.message_id`. Result intents delegate to existing `report_result_for_owner_team` with a fixed `result_id`, preserving real task finalization and natural leader presentation. No arbitrary tool-supplied workspace/owner/sender.
- A forwarding intent is write-ahead state, not proof of final queue acceptance or presentation. Preserve pending/in-flight/accepted/refused/unknown; never automatically replay unknown. Reconcile only by exact ID + original scope/recipient/content. Root idempotency IDs must be globally safe across K3 scopes and store reincarnations (current K3 local counters alone are not).
- Public status/inbox must aggregate actual backend facts, not fabricate roots/duplicate physical queues or call two independent Teams “mixed”. Generic root state needs a contract binding reference and truthful status; keep its legacy `pane_id` absent so old cleanup/layout cannot touch another socket's same numeric pane.

A possible alternative (not selected) was adapting K3 to the legacy physical queue. It would require a larger queue/storage-trait refactor and new root DB generation-binding rows; the per-recipient ingress/forward-intent seam is smaller and preserves the existing verified K3 path.

## Read source facts

- `messaging/send.rs::send_message` is shared by CLI/MCP, handles leader/fanout/membership, then the legacy worker path. `SendOptions.message_id` is a documented caller idempotency key. `TrustedSender` stores a bare agent ID plus optional qualified display name; use the trusted identity, not a tool argument or a guessed split of a displayed address.
- `messaging/persist.rs::persist_resolved_send` is the existing durable-message entry. `db/message_store.rs::message_exists` and exact-record reads support reconciliation. A duplicate ID alone is NOT proof that original payload/scope match.
- `messaging/results.rs::report_result_for_owner_team` accepts an envelope with a fixed result_id, validates it, inserts idempotently, finalizes tasks and routes real leader notification. Results persisted in K3 must not silently omit this common business lifecycle for mixed teams.
- `mcp_server/tools.rs::TeamOrchestratorTools::with_identity` takes framework-bound agent and owner-team, not a provider enum; useful for read-only common status projection. Do not bypass the new native PID/instance/connection guards by invoking it with arbitrary strings.
- `messaging/delivery.rs` has central `deliver_pending_message`/`deliver_prepared_message` paths. Do not let unknown Kiro reach old provider parsing. Old DB rows contain sender/recipient/content/task/owner/status but no generic native generation binding; do NOT silently retarget a Kiro message based only on current root state.
- `lifecycle/launch/spawn.rs::spawn_agents` currently parses provider then defaults to Codex. New registry dispatch must precede that parse, not add Kiro to the old enum or reach fallback.
- `lifecycle/launch/agent_state.rs` is legacy typed-provider state construction. `state_projection` needs a new-runtime branch before invoking it. Existing `StartedAgent` has no provider enum, but its target is currently legacy-oriented; layout/cleanup must not interpret a new backend target as a legacy pane.
- `compiler::compile_role_agent_with_mode` currently resolves effort through a parsed legacy provider (unknown → Codex); Kiro must bypass that legacy policy and use H1, without changing old provider results. `model/spec.rs` has generic provider admission checks for leader and members; registry extension belongs there, not in provider internals.
- `orchestration/physical.rs::PhysicalRuntime` is a short-lived operation context and restores owned targets from `SeatRecord.physical`; it has no queue or scheduler. A daemon and operator CLI can reopen the same captured target with fresh host checks. No new IPC protocol is strictly needed for DB enqueue/status; lifecycle concurrency still uses existing K3 leases.

## Required new-contract substrate next

1. Framework-owned external-peer/result routes scoped to the native store, separate from native seats. Root authoritative membership supplies them; tools cannot write them. Broadcast/status must include real mixed members. Absent external routes retain standalone K3 semantics.
2. Transactional forwarding intents for MCP send/result, exact ID mapping and closed outcome states. Do not hold a K3 write transaction while calling the root messaging/lifecycle port (avoid cross-store lock cycles).
3. Framework ingress allowing a trusted captured sender, unlike `send_from_operator`'s fixed leader; every Kiro target resolves in the same transaction as native outbox admission. Qualified/Unicode legacy identity needs deliberate display/address handling, not fabricated provider/seat identity.
4. Root service/entry: scope-owned supervisor, owned stdio MCP subprocess, real per-frame parent-native-process and instance checks, and a scoped lifecycle/control channel. No hidden fake mode in production.
5. Generic root dispatch/state/lifecycle integration (cold start, add/start/stop/restart/remove/clone/fork/status/send/shutdown). Existing provider implementations remain frozen. Fresh clone must have independent config and no copied session/bootstrap/transport receipt. Unsupported native Fork/resume remains typed, not disguised as fresh.

## Native evidence still blocks honest admission

H4/H5/H6/H7 remain Unverified. Help and owned-config validation do not prove native selection/consumption, SID, input recipes, or client registry. Leader was asked via msg_af51a7c106b8 for real V3/TUI empty/busy/paste/submit, bypass keys, /session-id, and live client /tools/registry evidence plus actual wire names.

- Fresh capture may require a declared SessionInspect control; do not bind by newest session.
- MCP can start before F3 target publication. The stdio resolver must bounded-wait for the actual target/parent witness rather than falsely reject init or fabricate a process.
- A business frame can arrive near delivery receipt commit. Resolve task from the exact submitted generation, bounded-wait if necessary; queued/mailbox/uncertain entries are not synthetic native turns.
- tools/list response-written is T3a, not T3b. Do not label all tools bound merely from a server row or mint fresh timestamps on old proof.

## Leader wrapper

The required public entry is `$BIN kiro -- <native argv>`, not an agents/leader.md worker. It needs a real owned native caller binding; preserve original token vector and distinguish existing leader, attachment and actual re-exec. Worker-team restart must not re-exec the caller or leak leader argv to workers. Managed agent/MCP/harness conflicts need explicit refusal, not dropped flags. A leader's native scope may outlive a worker Team and must not be killed by that Team's scoped shutdown.

## Current validation boundary

Leader reports 7f3b6688 contract 165P/0F/0I + clippy/fmt0. c392d632 adds root config/prompt/registry + eight tests only; builder is assigned root lock and targeted verification. No public runtime wiring, no native consumption, no mixed business PASS yet. No local Rust tools/native developer probing. Current task has not called report_result.
