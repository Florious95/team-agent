# Kiro public CLI — active implementation

## Authorization / current stage

Leader `msg_e89e6e0bec68`: implement the public CLI and real native bridge, then Grok validation, official macOS arm64 candidate, isolated basic quick-start/send/real-reply/shutdown acceptance before release. Release owner is base-sol, target 0.5.114; this developer does not merge/tag/publish. Sonnet-4.5 is the explicit tested-provider exception; caller/runner stay Luna. No local Mac Cargo/rustc/rustfmt or developer native probing.

Leader `msg_3868f9336ab8` approves **isolated generic contract-runtime routing** for standard quick-start/send/status/collect/shutdown; Kiro scopes use K3's single outbox + Supervisor + MCP. Existing providers retain their path. Mixed Kiro/legacy providers in one scope must fail in F0, before runtime writes/spawn. Do not maintain two queues for a seat or silently run through a legacy adapter.

Stage: source tracing and implementation. Not a candidate, not native PASS.

## Source / ownership

- Worktree: `/Volumes/nvme/Builds/team-agent-worktree-kiro`.
- Branch: `feat/kiro-contract-public-cli` (new). PR307 / `feat/kiro-contract-integrated` stays frozen at `8d13090d7f805741cc4ce25bf9296846bc611a86`.
- New branch base merge: `f56e12d8094366044449019f65e48e622b3cf14b`, combining the verified H1 substrate with current main `f538911c` (0.5.113), including the real quick-start fixes from #302/#304. Do not regress to 0.5.112 root CLI.
- Earlier 159P/0F/0I + clippy/fmt0 validates only `fcbbc752`, archived under `kiro-catalog-auth/`. New product changes invalidate affected evidence; no new tests run yet.

## Read facts / planned join points

- Root crate is `crates/team-agent`, NOT root `src/`. Public CLI dispatch: `src/cli/emit.rs`; typed argument parsers and command adapters already exist. `main.rs` has early `mcp-server`/`fake-worker` branches. Current public replies use `inbox`; collect is not currently a public legacy command.
- Main's `lifecycle/launch/quick_start.rs` owns leader-only initialization and source-scoped leader binding. Contract dispatch must not interfere with no-Kiro/empty-role legacy paths.
- Reuse `compiler::read_front_matter` / existing YAML parser for role content; add the contract path without widening the legacy provider enum through every legacy match. New scope validation must reject mixed providers and invalid/unverified model/effort before runtime effects.
- Contract crate has independent `[workspace]`/lockfile. Root path dependency must explicitly preserve that boundary (workspace exclusion/Unix cfg as needed) and be lockfile-checked on Grok, not locally.
- K3 `ContractStore`, `Lifecycle`, `supervisor::tick`, `mcp::serve` and `PhysicalRuntime` are actual library implementations, not a public process service. Operator enqueue, bound task/context resolution, current protocol facts and bounded subprocess/IPC lifecycle still need a generic bridge. Reuse the one `contract_outbox`, not a Kiro queue.
- K2 `TmuxHost` uses per-instance owned endpoints; Mac socket path must be <104 bytes. Do not put long workspace paths into private sockets or adopt shared/default tmux. Physical receipts survive restart but need fresh identity rechecks.
- Native MCP context must be derived from the owned scope/instance/generation and real parent/native process, not tool-supplied sender/task or a cloned Fixture label. Server handshake is not client binding; result durable is not automatic presentation.

## R0 evidence / edits in flight

- `/Volumes/nvme/tmp/kiro-native-receipts/r0-deep-prep/REPORT.md`: agent/mcp help exit0; mcp list reports default/workspace groups on stderr, but configuration paths/precedence/consumption remain UNKNOWN. Chat help confirms `--require-mcp-startup` exit3 on enabled-server failure and `--agent-engine` (v1/v2/v3), with `--v3` already supported.
- Added `--require-mcp-startup` to Kiro plan and updated bounded fixture assertions. Native admission remains closed pending actual profile/session/MCP evidence.
- Leader `msg_fbe34e4ab498` / runner result `res_da5e094e6f39`: `agent validate --help` requires `--path <PATH>`; no `show` subcommand (exit2). Corrected owned validator argv and added a captured-runner path/space/Unicode regression. Raw receipt location for this new item is not yet supplied; do not claim a source file was inspected.
- Leader has assigned runner further R0 collection. No developer execution/login/credential reads. Need native input surface/submit/SID and actual owned config/client-binding proof, not just help.

## Next work

1. Generic operator ingress is implemented, pending Grok validation: `orchestration/operator.rs` shares one transaction-owned message primitive with MCP send/report, resolves each accepted target generation, adds submitted-task and exact-connection fact queries, and exposes a read-only inbox. `mcp::serve_with_context` retains bounded framing/response-written truth while allowing framework-owned per-frame context resolution. Five controlled tests plus the validator regression are added; no native/UI readiness is synthesized.
2. Implement isolated public config/runtime routing and owned supervisor + MCP subprocess entry. Unsupported commands/scopes must refuse, not fall through to legacy mutations.
3. Bind real native hooks only from R0 evidence; keep unrelated Fork/resume capabilities explicit if not established.
4. Push exact source to builder; obtain real root + contract test/clippy/fmt results and official arm64 candidate. Prepare clean four-step basic case only after real candidate/config are frozen; no placeholder dispatch.

This is an in-progress ledger, not a completion report. Current task has not called report_result.
