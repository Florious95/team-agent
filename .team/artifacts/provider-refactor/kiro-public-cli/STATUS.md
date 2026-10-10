# Kiro public CLI — active implementation

## Authorization / current stage

Leader `msg_e89e6e0bec68`: implement the public CLI and real native bridge, then Grok validation, official macOS arm64 candidate, isolated basic quick-start/send/real-reply/shutdown acceptance before release. Release owner is base-sol, target 0.5.114; this developer does not merge/tag/publish. Sonnet-4.5 is the explicit tested-provider exception; caller/runner stay Luna. No local Mac Cargo/rustc/rustfmt or developer native probing.

**Superseding authorization:** leader `msg_6fd7d38a2358` now requires **per-agent routing and real mixed teams (B03 positive)**. Kiro roles use the new contract lifecycle/materialization/physical execution/communication; legacy roles retain their existing provider internals, which stay frozen. The earlier `msg_3868f9336ab8` whole-team separation/F0 mixed-team refusal is explicitly superseded. Do not maintain two physical delivery queues/executors for one seat or disguise Kiro as a legacy provider.

Leader `msg_eabe5104ced0` now requires real Kiro Leader launcher + native argv preservation (not an Unsupported substitute), dynamic model/effort response without drift, independent configuration clone, typed supported/refused Fork, and public quick-start/send/status/scoped shutdown. New public specs are `/Users/alauda/Documents/code/agent前沿探索/多agent协作/.team/artifacts/provider-refactor/KIRO-FULL-USER-SCENARIO-SPECIFICATION.md` (169 cards) and `KIRO-EXHAUSTIVE-LOGICAL-FEATURE-MATRIX.md` (231 points). They are public requirements, not hidden red-test code.

Prompt authority: `msg_e4ef076b4120` permits the standard optional user `~/.pi/agent/AGENTS.md` or unified user setting, project-root AGENTS, role front matter/body, TEAM explicit instructions. Latest order in `msg_eabe5104ced0`/`msg_6fd7d38a2358`: **user → project → role → TEAM explicit**, then dynamic tasks through the existing delivery pipeline. Paths belong in a central framework source policy, not the Kiro adapter; never modify those source documents.

Stage: source tracing and implementation. Not a candidate, not native PASS.

## Source / ownership

- Worktree: `/Volumes/nvme/Builds/team-agent-worktree-kiro`.
- Branch: `feat/kiro-contract-public-cli` (new). PR307 / `feat/kiro-contract-integrated` stays frozen at `8d13090d7f805741cc4ce25bf9296846bc611a86`.
- New branch base merge: `f56e12d8094366044449019f65e48e622b3cf14b`, combining the verified H1 substrate with current main `f538911c` (0.5.113), including the real quick-start fixes from #302/#304. Do not regress to 0.5.112 root CLI.
- Earlier 159P/0F/0I + clippy/fmt0 validates only `fcbbc752`, archived under `kiro-catalog-auth/`.
- FF'd builder formatting `eb5a145a`; validator fixture fixes `d2de2ecb` and `7f3b6688`. Leader `msg_eabe5104ced0` reports **7f3b6688: 165P/0F/0I; clippy0; fmt0**. These are contract-crate tests; raw receipt path has not yet been received/inspected by this worker. New root/prompt/routing changes are NOT RUN.

## Read facts / planned join points

- Root crate is `crates/team-agent`, NOT root `src/`. Public CLI dispatch: `src/cli/emit.rs`; typed argument parsers and command adapters already exist. `main.rs` has early `mcp-server`/`fake-worker` branches. Current public replies use `inbox`; collect is not currently a public legacy command.
- Main's `lifecycle/launch/quick_start.rs` owns leader-only initialization and source-scoped leader binding. Contract dispatch must not interfere with no-Kiro/empty-role legacy paths.
- Reuse the existing YAML/front-matter parser without widening the legacy provider enum. `compiler::split_front_matter` now exposes the same pure parser so metadata and byte-preserved source derive from one captured input; old `read_front_matter` keeps its behavior. Per-agent routes must support mixed members; invalid/unverified Kiro model/effort must fail before effects.
- Contract crate has independent `[workspace]`/lockfile. Root path dependency must explicitly preserve that boundary (workspace exclusion/Unix cfg as needed) and be lockfile-checked on Grok, not locally.
- K3 `ContractStore`, `Lifecycle`, `supervisor::tick`, `mcp::serve` and `PhysicalRuntime` are actual library implementations, not a public process service. Operator enqueue, bound task/context resolution, current protocol facts and bounded subprocess/IPC lifecycle still need a generic bridge. Reuse the one `contract_outbox`, not a Kiro queue.
- K2 `TmuxHost` uses per-instance owned endpoints; Mac socket path must be <104 bytes. Do not put long workspace paths into private sockets or adopt shared/default tmux. Physical receipts survive restart but need fresh identity rechecks.
- Native MCP context must be derived from the owned scope/instance/generation and real parent/native process, not tool-supplied sender/task or a cloned Fixture label. Server handshake is not client binding; result durable is not automatic presentation.

## R0 evidence / edits in flight

- `/Volumes/nvme/tmp/kiro-native-receipts/r0-deep-prep/REPORT.md`: agent/mcp help exit0; mcp list reports default/workspace groups on stderr, but configuration paths/precedence/consumption remain UNKNOWN. Chat help confirms `--require-mcp-startup` exit3 on enabled-server failure and `--agent-engine` (v1/v2/v3), with `--v3` already supported.
- Added `--require-mcp-startup` to Kiro plan and updated bounded fixture assertions. Native admission remains closed pending actual profile/session/MCP evidence.
- Leader `msg_fbe34e4ab498` / runner result `res_da5e094e6f39`: `agent validate --help` requires `--path <PATH>`; no `show` subcommand (exit2). Corrected owned validator argv and added a captured-runner path/space/Unicode regression. Raw receipt location for this new item is not yet supplied; do not claim a source file was inspected.
- R0 V2 report/raw captures read: `/Volumes/nvme/tmp/kiro-native-receipts/r0-interactive-v2/REPORT.md`, 06/09/12/20. Actual startup omitted --v3; session JSON explicitly source=v2. It proves V2 `›`, single Enter on short ASCII input, UUID session line, 14 built-ins; NOT V3, owned Team MCP, bypass, multiline paste, or Fork. Correction sent in `msg_d746559da158`.
- R0 V3 report read: `/Volumes/nvme/tmp/kiro-native-receipts/r0-v3/REPORT.md`. Actual --v3 --tui --trust-all-tools displayed a risk dialog, default `❯ No, exit`. Runner selected No with one Enter and exited0. Screen explicitly says ↑↓ navigation; **Tab/Yes acceptance were NOT observed**. Exit hint has `sess_<UUID>`; this is NOT a /session-id result. V3 ready/tools/business remain NOT-RUN. A supported consent recipe must verify `Yes, I accept` focus (not “don't ask again”) before one Enter and only on explicit bypass intent. Do not infer V3 grammar or unlock Fork from these receipts.
- No developer execution/login/credential reads. Native bindings stay closed until exact-profile evidence exists.

## Next work

1. Generic operator ingress (`orchestration/operator.rs`) + `mcp::serve_with_context` are in the 165P reported baseline. The validator fixture now selects LaunchOnly and omits FullWorker MCP argv/carrier; explicit plan validation exposes actual errors before the host's coarse Ownership wrapper. No production gate was weakened.
2. Root foundation is in flight: Unix-only path dependency preserving independent contract workspace/lock; `contract_runtime/{config,prompt,registry}.rs`; five config and three prompt tests. Config has per-agent family routing (legacy definitions unchanged), complete role-document snapshots, central user policy override `TEAM_AGENT_USER_INSTRUCTIONS`, workspace-root AGENTS, and ordered TEAM file/inline instructions. Prompt receipts carry full content hash/size/byte ranges; native prompt admission/consumption is NOT yet wired/proven. No public dispatch/service exists yet.
3. Joining direction is recorded in `PER-AGENT-JOIN-DESIGN.md`: Kiro ingress goes to K3 once; old recipients stay on old queue. Native egress uses durable forwarding intents (NOT a second physical queue) and common send/result APIs with exact IDs. Five new contract tests and `orchestration/forward.rs` are in flight: framework peer/result routes, captured immutable intent route, pending→in-flight→accepted/refused/unknown with no implicit unknown replay; source framework sender; mixed roster/broadcast; JSON semantic idempotency under root preserve_order. New native DB schema3 adds a random store incarnation so generated IDs do not collide across stores/re-creation. Source changes NOT RUN. Concrete root forwarding/service/public dispatch are still pending. Do not create a second whole-team facade and claim B03.
4. Kiro Leader target is the real public `$BIN kiro -- <native argv>` wrapper (spec §3.1), not an `agents/leader.md` worker. Preserve original token vector, distinguish already-running/attach/re-exec, and keep native flags from leaking to workers. Existing `cli/leader.rs` is the generic entry; its current provider enum path cannot silently default Kiro.
5. Bind real native H4/H5/H6/H7 only from R0 evidence (profile/submit/SID/owned MCP consumption). Fresh SID may need a declared bounded SessionInspect control before H4; current PhysicalRuntime capture cannot invent a SID from newest sessions. Keep native capabilities closed until evidence is supplied.
6. Push exact source to builder; root lock update + targeted root tests + parser/runtime-prompt regression + contract tests under root's serde_json feature unification are needed. Do not run Rust tools locally. Do not let whole-workspace rustfmt rewrite frozen unrelated provider files. Official arm64 candidate/basic and mixed native cases follow complete dispatch, not placeholders.

This is an in-progress ledger, not a completion report. Current task has not called report_result.
