# Kiro integrated — delivered substrate stage / native work blocked

## Authorization / stage

Leader authorized K1–K4 integration, captured WorkingDirectory grant, production-native evidence work, automated validation and eventual K5. No merge/tag/release authorization. Native preparation/acceptance belongs to Luna runners; Cargo/fmt/clippy belongs to Grok/authorized CI. Do not run Kiro or inspect credentials from the developer seat.

## Candidate / ownership

- Worktree: `/Volumes/nvme/Builds/team-agent-worktree-kiro`
- Branch: `feat/kiro-contract-integrated`
- Ready for Review (OPEN, isDraft=false): https://github.com/Florious95/team-agent/pull/307
- Product-code freeze: `12ef1aafb1cd2031894d73c6511428bc72b4c1a2` (physical bridge fd453234 + builder fmt cc508177 + targeted Rust lifetime fix).
- Final tested head: `be67a0128dc06e6692e095d92bb19252c642145e`, tree `5015793824f894594c169ec13cd21e3c1f02f3fc`; 91533932's rfind test-only edits + builder formatting. **152 PASS / 0 FAIL / 0 IGNORE, test/clippy/fmt and CommandExit all0**. Product src/manifest/lock byte bridge to12ef = diff0.
- Leader authorized Ready for Review + final receipt (message msg_d96e5651cf48). This closes the integrated-substrate delivery stage, not native Kiro/K5 acceptance.
- Builder-owned branch: `builder/kiro-contract-integrated-29c43fd-a1`; builder returns only committed fmt/lock changes. Do not edit its checkout/cache.
- Preserved K1 frozen input `2e43ad02`, K2 `fe4f1761`, K3 PR305, prior gated K4 PR306. No old provider/root manifest/AGENTS/CLAUDE changes relative to K1 (git diff exit0 checked).

## Valid evidence vs current pending work

- Leader/builder reported predecessor `8a53aa8310c10bda51045111582013a68643fa3e`: 147 passed/0 failed/0 ignored. Two nonminimal_bool clippy issues fixed by `129a874e`. The verified original receipts are now archived beside REPORT.md; predecessor PASS is kept separate from the final all-green run.
- Prior first failure: `kiro_catalog` fixture used Rust `\033` (NUL+33), so runner correctly returned NotStarted. `2e35cb48` changed to raw string and asserted no NUL; expected OutputRejected was preserved. Later 147-test run passed it.
- `fd453234` added PhysicalRuntime/target persistence and 5 tests. Builder fmt head `cc508177cd009c6e95de7f6164ff8b3addb6a9ab` failed to compile with 7 lifetime diagnostics (no new suite run). Root cause: mutable bootstrap trait object's long object lifetime was incorrectly the same parameter as short host/input/policy/journal borrows. `12ef1aaf` separates these two lifetimes in InjectionRequest/Run and removes the redundant closure; no static/leak/unsafe workaround, no weakened tests. Builder's `gb-12ef1aaf-contract-validation-a1` subsequently ran all **152 PASS / 0 FAIL / 0 IGNORE**, TEST_EXIT=0; Clippy failed only two test filter-next lints, fixed by91533932. Final `gb-be67a012-contract-final-a1` then passed all152 + clippy/fmt (exit0). No Native/K5 claim.
- Actual compile-failure receipts read at `/Volumes/nvme/tmp/grok-bot-evidence/gb-cc508177-contract-final-a1/`; CommandExit=101. Previous 147-pass receipts at `gb-8a53aa83-contract-final-a1`; auth fixture first failure at `gb-4ffdf965-contract-validation-a1`; 134-pass predecessor at `gb-09160774-contract-verify-a1` (same evidence parent). Do not infer wipe from local cleanup; fetched status can still say Wiped=no. These historical receipts are archived without rewriting status; final/fmt-prep post-wipe proof is separate.
- Builder must preserve real tmux fixture receipts/roots, prove exact native/server exit, archive and receipt-bound clean owned resources. Do not broad-kill or remove the shared target/cache.

## Native facts / blocker

Raw runner evidence: `/Volumes/nvme/tmp/kiro-native-receipts/`, including `chat-subcommands/REPORT.md`.

- Dispatcher root version/help pass; subcommand wrapper fails looking under `~/.local/bin` even with PATH prefix.
- Direct helper `/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat` version2.28.0/roothelp/chathelp exit0; SHA256 `430aae1a4f5ae252e785114d45d61ec465796ad6ca96383fb7ca383dd96b1712`.
- `chat --list-models --format json` partial101586-byte stdout is **not JSON**; repeated `Opening auth portal and logging in...` spinner. Outer180s timeout, native exit unknown. No valid catalog/schema or Luna ID. Developer explicitly corrected the optimistic misinterpretation; leader authorized AuthRequired classification and no login/credential reads.
- Agent/MCP help stopped NOT-RUN after catalog first error. No native terminal prompt/paste/timing/SID/resume/rewind/MCP roundtrip evidence.
- No further native retry is authorized while authentication prerequisite is unresolved. Closing stdin/cancelling marker cannot undo a browser opened earlier by Kiro.

## Implemented / remaining

See package `docs/kiro-adapter.md`, `docs/physical-lifecycle.md` and `README.md`.

Implemented: K1–K4 composition, descriptor+captured cwd grants and exact file cleanup token, direct-helper discovery, bounded auth-required reads, native-help argument topology, generic physical lifecycle/outbox bridge, persisted actual routes, strict restore, one-store bootstrap reservation, common renderer/executor/control/stop, controlled fixtures.

Not completed: production Kiro H4/H5/H6/H7/profile data, authenticated exact Luna catalog, public supervisor/MCP subprocess entry and native connection provenance, macOS candidate, full blind K5. Do not call the library integration or fixture tests a finished Kiro product. Native H2 retains its fail-closed admission barrier. FullSnapshot/NativeNewSeat remain Unsupported.

## Next action

be67a012 has been FF'd and pushed; PR307 was verified OPEN/isDraft=false on that head. Final raw receipts + independently read-only post-wipe statuses for final/fmt-prep units are archived; both Wiped=yes. The evidence-only commit preserves tested package bytes; final report uses this archive. Remaining Native/R0/public-entry/K5 limits are in REPORT.md; no login/native retry/merge/release. Await a new actionable instruction after this stage handoff, rather than inventing prompt/SID/Fork semantics.
