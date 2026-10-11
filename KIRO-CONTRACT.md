# Kiro contract-runtime candidate

This integration is a **candidate for native acceptance**, not a claim that the full user-scenario matrix has passed. Existing providers keep their adapters and physical queues. Kiro is selected per worker through the independent `crates/team-agent-contract` package; it is not an alias for Codex or Pi.

## Public entry

Use an existing, authenticated Kiro installation. No login, account-database copy, API-key mapping, global configuration rewrite, or model substitution is performed.

A role document under the team's `agents/` directory:

```markdown
---
name: worker
role: Kiro worker
provider: kiro
model: claude-sonnet-4.5
effort: high
dangerously_skip_permissions: false
---
Perform the assigned task. Report completion through the bound Team Agent tools.
```

Use the normal public `quick-start`, `status`, `send`, and scoped `shutdown` commands. Run `team-agent models --provider kiro --json` to inspect exact native IDs. Catalog visibility does not prove subscription access; explicit native effort flags do not assert per-model reasoning quality.

`doctor` includes Kiro in its provider inventory on the contract-runtime platform. `installed` only means an executable `kiro-cli` was found on PATH; auth/version/readiness remain unprobed. `team-agent kiro --help` documents the separate leader entry. `team-agent kiro [--json]` currently returns exit 1 with `reason: kiro_leader_not_admitted` (`leader_launch.v1`), without starting a process or binding a leader. This is an explicit capability refusal, not a working leader launcher.

The current transport recipe is macOS **V2/TUI**, initially observed on Kiro 2.28.0; it is not a release-version or distribution-hash allowlist. Both native banners are parsed as a complete numeric `major.minor.patch`, including 2.29.0 and future releases. Malformed banners are reported with their escaped, bounded actual output. Catalog schema and current terminal observations still have to match; a future incompatible UI is not treated as ready. `--v3` is not used.

The launcher is resolved to `kiro-cli-chat`, including the official application-bundle location when the Homebrew dispatcher cannot locate its helper. Discovery captures the actual version and binary hash, checks that the image did not change during the version probe, and binds that identity to the new generation. Catalog, materialization, spawn, session and physical input retain their exact runtime identity fences. A running generation cannot adopt a replacement binary or reuse a policy resolved for another version/hash. A subsequent fresh launch captures the new installation; no source-code release/hash update is required.

## Instruction fidelity

The framework resolves and copies complete UTF-8 source documents in this order:

1. User-global instructions (`TEAM_AGENT_USER_INSTRUCTIONS`, or the existing user `AGENTS.md` compatibility location).
2. The explicit project workspace's `AGENTS.md`.
3. The complete role document, including its original metadata and line endings.
4. `TEAM.md` and explicitly ordered `instruction_files` / file-list `instructions`.

Framework worker identity, communication mode, and the three-tool runtime contract precede these layers. Each source has a hash and exact byte range. Oversize input is rejected, never truncated. A private startup snapshot binds the selected role/model/effort and assembled prompt. Dynamic messages enter the durable outbox once; they are not duplicated in startup argv.

Each native instance receives an exclusively created `.kiro/agents/<owned-name>.json` with its own prompt and stdio `mcpServers` binding. The shared workspace `mcp.json` is **not overwritten**: doing so would cross-wire identities for workers sharing a directory. Ambient MCP/power inclusion is disabled in the generated agent configuration. Only the three collaboration tools request unattended permission. The default never passes `--trust-all-tools`.

## Lifecycle and physical contract

- Team-wide native configuration/model admission occurs before shared worker spawning.
- K3 journals materialization, registration, spawn, exact current-session capture, and commit/compensation. K2 owns the private tmux target, executable/birth identity, input lane, effect journal, and scoped teardown.
- An uncertain spawn/control retains its lifecycle lease. Shutdown then reports `NeedsRecovery`, not a database-connection failure, and preserves resources whose process effects are unproven. It never clears that lease or guesses a target merely to report successful cleanup.
- `/session-id` must produce one labelled UUID and the matching native resume hint. Session listings, newest files, process liveness, and arbitrary UUIDs are not session bindings.
- Business input uses a bracketed payload with no added trailer and **one Enter**. There are no confirmation, retry, wrap-gap, or native-queue-flush keys. Multi-line paste/acceptance remains a candidate behavior to be verified against the real binary.
- The current composer is `›`; `Thinking` / `Kiro is working` are busy observations. A vanished paste alone is not acceptance: the current token must be observed in the native transcript, with the exact message/attempt association.
- The coordinator is the sole K3 outbox consumer. CLI/MCP ingress queues work; `queued` does not mean submitted. Uncertain effects are fenced rather than replayed.
- Kiro private `%0` values are never published as legacy pane targets. The normal status projection samples native process identity, keeps unobserved activity/health unknown, and provides an endpoint-specific attach command.

## MCP evidence and return path

The stdio child must match the captured candidate binary, process birth, native ancestry, scope, instance, and generation. Tool arguments cannot set sender, task, owner team, or transport identity.

Native ancestry permits at most **three parent edges** (two intermediary helpers). Both captured endpoint processes must remain alive with the same birth/image/parent; intermediary birth, parent and executable path are sampled during the bounded walk and rechecked in reverse. Missing, exited, replaced, cyclic, deeper or unreadable chains fail closed. This is not an executable-name allowlist or permission to adopt an arbitrary Kiro process.

Entry, connection registration, every MCP call context, and coordinator protocol sampling use the same live verifier; a persisted connection is never a cached ancestry grant. No store-schema migration or rewrite of native/candidate identity is needed. `contract.mcp` diagnostics distinguish `direct_parent_matches` from the verified `ancestor_chain` / `max_parent_hops`; diagnostics are not T3 protocol facts.

T1 (native process), T2 (current composer), and T3 remain distinct:

- T3a: initialize/tools-list responses actually written on a live owned connection.
- T3b: the current native `/tools` table exposes all three tool names from the exact `mcp:team` source. The inspection has its own seat lease; its single Escape close is separately guarded/journaled and never counts as a business submission. A partial/scrolled-off table is not promoted.
- T3c: actual native tool calls/results. Server writes, registration, persistence, client consumption, and leader presentation are not interchangeable facts.

The R0 external-server experiment established two **mock** tools in the real Kiro registry. It did not invoke them or prove this candidate's three-tool return path. Acceptance must exercise the generated owned agent, current registry, native calls, and natural leader presentation.

Kiro-to-legacy/leader messages and results use durable forwarding intents with fixed IDs. The forwarder delegates to existing shared messaging/result APIs, including the existing result-envelope normalizer and task finalization. Unknown forwarding outcomes are not automatically replayed.

## Explicit limits

The implemented public path is initial worker `quick-start`, read-only `status`, durable `send`, and scoped `shutdown`. Native leader wrapping, restart/resume, mutable/add/remove worker lifecycle, clone/Fork, and automatic startup-risk consent are not admitted by this candidate. Public lifecycle guards prevent these requests from falling into the historical unknown-provider fallback. An explicit blanket bypass request is refused until its native consent sequence is admitted.

No native acceptance or macOS build is implied by grammar fixtures. Release requires the exact-source Grok checks, an official macOS candidate, and independent native scenario receipts. No existing provider is migrated by this change.
