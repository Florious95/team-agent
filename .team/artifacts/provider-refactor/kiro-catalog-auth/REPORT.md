# Authenticated Kiro catalog — H1 verified delivery

**Delivered:** H1 and its automated verification. **Not delivered:** native Team Agent startup/send/MCP/K5 acceptance. Leader final validation result: `res_d84379fa2190`; handoff scope confirmed by `msg_a701be8cc5f8`.

## Task and boundary

Leader messages `msg_ae4d580add80` / `msg_e6f31fb10e50` authorize alignment with the authenticated raw catalog and automated validation. `msg_d63a82ab75d8` explicitly names `claude-sonnet-4.5` for the next native case; `msg_a701be8cc5f8` confirms delivery of K5 admission requirements rather than a fictitious candidate. Worktree `/Volumes/nvme/Builds/team-agent-worktree-kiro`, branch `feat/kiro-contract-integrated`, PR [307](https://github.com/Florious95/team-agent/pull/307), OPEN / Ready for Review. No merge/release, credential inspection, developer-seat native execution or other model substitution.

This task follows the frozen `integrated/` receipt archive; it does not rewrite that earlier stage's authentication blocker. Parent `85f1766db8456a39a00906ce823abe6bd0deb18a` archives the 152-pass tested baseline `be67a0128dc06e6692e095d92bb19252c642145e`. New H1 product bytes require new validation.

## Exact evidence

Read-only source: `/Volumes/nvme/tmp/kiro-native-receipts/live-auth/`. Only its `catalog.stdout`, `catalog.exit` and `engine-identity.txt` are archived byte-for-byte under [native/](native/); no account/login output is copied.

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

Seven new controlled tests cover the actual catalog, strict schema/selection, invalid grants, output failures, and CatalogReader → bound H1 composition; existing admission expectations updated. They never invoke native Kiro. No local Cargo/rustc/rustfmt was run. Local checks: `git diff --check`, exact fixture byte/hash comparison.

- Implementation `a62e5ae6502a9a421f3e74168501d603bab48592`, followed by builder formatting `90c2d553575489cc30057c87141d4fcf63e76ae9`, tree `fc8ea8417e366b200b5af16bdf31f32bae823a8f`. Original fmt-prep unit `gb-a62e5ae6-contract-fmt-prep-a1` exited 0; verified receipts and separate Wiped=yes/no-process observation are also archived.
- `gb-90c2d553-contract-final-a1`: **159 passed / 0 failed / 0 ignored**, test exit 0. Clippy exit 101 for one unused test import (`descriptor::*`); fmt-check not reached. Overall CommandExit/ExecMainStatus 101. Original receipts retained under the exact unit directory; `artifact-sha256` verified. Separate read-only post-wipe status confirms Wiped=yes and no MainPID/Process; original pre-wipe status is not rewritten.
- **Final tested head:** `fcbbc752c594e0f8107f1c047145d03db5f75244`, tree `667db1bd937de84212816dc12e32deeea6b74c78`. This removes only the unused test import; product src/manifest/lock diff from 90c2d553 is zero.
- `gb-fcbbc752-contract-final-a1` actually reran the whole suite: **159 passed / 0 failed / 0 ignored**, 14 test targets including the zero-test library target. Test, Clippy `-D warnings`, fmt-check, CommandExit and ExecMainStatus all **0**. Rust/Cargo 1.95.0; exact commands, toolchain/cgroup receipts and logs archived beside this report. This is a fresh run, not just a source bridge.
- `gb-fcbbc752-contract-fmt-prep-a1` completed with exit 0 and no formatting commit required. Both final units' original `artifact-sha256` manifests verify; separate read-only `status.sh --no-install` captures independently confirm terminal/success, Wiped=yes, empty MainPID/Process. The earlier 90c2d553 failed unit is also confirmed wiped, without changing its exit 101.
- Wipe proves these exact job units were removed, not broad deletion of retained fixture diagnostic roots or the builder's shared incremental cache `/workspace/grok-bot-offload/jobs/gb-6fae019-pr304-full-b2/work/target`. Those are not this developer's cleanup authority. No native Kiro process was started here.
- All new code remains within `crates/team-agent-contract`. Legacy `src/`, root manifests/lockfile and AGENTS/CLAUDE compare unchanged to frozen K1 `2e43ad02`.

## Remaining native acceptance gates

The new auth/catalog receipt clears those two earlier unknowns only. The catalog lacks Luna; the latest leader instruction now explicitly authorizes **only `claude-sonnet-4.5` for this K5 case**, with a Luna caller. This is not a global model-policy change, not an Auto fallback, and not permission to invent model-specific effort support.

Native terminal/session evidence, public supervisor/MCP process entry and a macOS candidate remain unavailable. H4/H5/H6/H7 stay Unverified and Native H2 stays closed; Fork's unsupported modes are unchanged. K5 remains **NOT-RUN**. See [PR-K5 admission and four-step acceptance](PR-K5-NATIVE-ACCEPTANCE.md), which separates preparer-only R0 collection from the later clean runner handoff. It explicitly contains no executable candidate/config pretending to exist.

Completion for this handoff: archive verified receipts, publish the accurate PR report and K5 admission specification. [VALIDATION.json](VALIDATION.json) records exact heads/results; `ARCHIVE-SHA256` covers all other archive files. The next implementation/preparation owner must close its gates and freeze a real candidate before dispatching the four-step blind case; no test has been launched from this document. This receipt/docs-only handoff preserves tested src/tests/fixtures/manifests/lockfile/workflow bytes.
