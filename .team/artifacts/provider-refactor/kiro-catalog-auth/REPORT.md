# Authenticated Kiro catalog — H1 landing

## Task and boundary

Leader messages `msg_ae4d580add80` / `msg_e6f31fb10e50` authorize alignment with the authenticated raw catalog and automated validation. Worktree `/Volumes/nvme/Builds/team-agent-worktree-kiro`, branch `feat/kiro-contract-integrated`, PR [307](https://github.com/Florious95/team-agent/pull/307). No merge/release, credential inspection, developer-seat native execution or model substitution.

This task follows the frozen `integrated/` receipt archive; it does not rewrite that earlier stage's authentication blocker. Parent `85f1766db8456a39a00906ce823abe6bd0deb18a` archives the 152-pass tested baseline `be67a0128dc06e6692e095d92bb19252c642145e`. New H1 product bytes require new validation.

## Exact evidence

Read-only source: `/Volumes/nvme/tmp/kiro-native-receipts/live-auth/`.

- `catalog.stdout`: 1719 bytes, SHA256 `7206950ed86df1f4835f38452981d1c0e28312fb7f2f292f43b828b08d295ce6`; `catalog.exit`: 0. Copied byte-for-byte into `crates/team-agent-contract/tests/fixtures/kiro-2.28.0-models.json`; `cmp` and SHA256 agree.
- `engine-identity.txt`: helper `/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat`, SHA256 `430aae1a4f5ae252e785114d45d61ec465796ad6ca96383fb7ca383dd96b1712` (unchanged observed 2.28.0 engine).
- Leader reports authenticated `chat --no-interactive` arithmetic returned 5535. `calc.exit` read as 0. No developer-seat execution; this is not TUI/SID/MCP evidence.
- `whoami.stdout`, credentials and account databases were not read/copied. No login was initiated.

The raw catalog contains **9**, not the message excerpt's 7, exact IDs: `auto`, `claude-sonnet-4.5`, `claude-sonnet-4`, `claude-haiku-4.5`, `deepseek-3.2`, `minimax-m2.5`, `minimax-m2.1`, `glm-5`, `qwen3-coder-next`. `default_model` is `auto`. There is no Luna and no per-model effort capability field.

## Implementation

- H1 bound to KiroAdapter; descriptor source schema `kiro-2.28.0-list-models-json-v1`. Existing bounded reader remains responsible for the captured command/cwd/image fence, closed stdin, output/time limits and owned-child AuthRequired guard.
- H1 validates request before host invocation and defensively checks host output bounds/exit. Direct typed deserialization rejects duplicate known fields, missing/invalid IDs/names/context windows, duplicate IDs, an empty catalog and a default absent from the catalog. No partial model results.
- Exact `model_id` is the only selection authority. No display-name/case alias, silent default, EOL filtering, price interpretation or invented effort support. All model efforts remain Unverified; explicit effort requests therefore remain fail-closed.
- NativeExistingSession is a supported authentication mechanism, not a cached assertion that the user remains authenticated. AuthRequired still propagates without retries/login.
- Existing H2 helper/argv plan is unchanged: model and five effort flag spellings are syntactically confirmed; list-only format is not sent to interactive chat. Native plan/profile/session/MCP gates remain closed. Seven hooks remain seven; no private queue or legacy-provider changes.

## Validation status

Seven new controlled tests cover the actual catalog, strict schema/selection, invalid grants, output failures, and CatalogReader → bound H1 composition; existing admission expectations updated. They never invoke native Kiro. **Rust tests/fmt/clippy for these new bytes are pending Grok; the prior 152-pass result is not reused as a new PASS.** No local Cargo/rustc/rustfmt was run. Local checks: `git diff --check`, exact fixture byte/hash comparison.

## Remaining native acceptance gates

The new auth/catalog receipt clears those two earlier unknowns only. Current native catalog lacks Luna; the existing caller/tested-Agent Luna-only rule needs an explicit user exception before any alternative model is used. No automatic Sonnet/Auto fallback. H4/H5/H6/H7, native terminal submit policy, public supervisor/MCP process entry and macOS candidate still need their own implementation/evidence. K5 remains NOT-RUN, not implied by library tests or noninteractive arithmetic.

Next: push H1 bytes → Grok fmt/test/clippy with exact returned SHA → archive actual result, then hand off the remaining native preparation to leader/runner without manufacturing evidence.
