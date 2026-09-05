# #129 provider 工具契约修复结果

## 交付身份

- worktree：`/Volumes/nvme/tmp/team-agent-tool-contract-repair-129`
- 分支：`fix/provider-tool-contract-129-luna`
- PR：[#133](https://github.com/Florious95/team-agent/pull/133)
- PR base：`release/0.5.74-startup-routing`
- 冻结 base SHA：`f2926d467f45d684549054bcc562607a255e6eea`
- 修复提交：`79d7930aeb65f3ad2987d554913f1fb1aaa70400`

## 根因与范围

installed CLI 0.5.72 对 Pi role 的 `provider_builtin` 只做通用 canonical 校验，`compile` rc=0 并把该类别写入 spec；Pi adapter 直到构造启动 argv 才拒绝它，因而 quick-start 可能已经持久化 runtime。

本 PR 仅修改：

- `crates/team-agent/src/provider/adapters/pi.rs`：复用现有映射，提供首个不支持类别查询。
- `crates/team-agent/src/compiler.rs`：Pi role compile 阶段拒绝映射不支持的类别。
- `crates/team-agent/src/model/spec.rs`：Pi leader/agent spec validate 阶段复用同一映射。
- `crates/team-agent/src/lifecycle/launch/quick_start.rs` 与 `crates/team-agent/src/lifecycle/tests/pi_compiler_red.rs`：增加 quick-start 持久化前拒绝和 compile 正反例。
- `skills/team-agent/SKILL.md`、`skills/team-agent/references/team-agent-operator.md`：明确 Pi 工具契约；Codex 的 `provider_builtin` 示例保留。

非范围：status、socket、recovery、版本 metadata、merge/tag/publish。

## 验证记录

- `git diff --check`：通过。
- installed `team-agent 0.5.72` 复现：Pi role（`model: openai-codex/gpt-5.6-luna`、`mcp_team`、`fs_read`、`provider_builtin`）执行 `team-agent compile --team ... --out ... --json`，退出码 0，生成 spec 含 `provider_builtin`；这是修复前证据。
- grok-bot-tests preflight：执行，成功；Rust/Cargo 1.95.0，cargo-fmt/rustfmt 1.9.0，nproc=8，远端可用磁盘满足门槛。
- grok-bot-tests sync：全新单次 unit `gb-79d7930a-pi-tool-contract-a1` 超时 180 秒；随后 `status.sh` 为 `State=missing`，未启动 Cargo。
- grok-bot-tests sync：全新单次 unit `gb-79d7930a-pi-tool-contract-a2` 超时 600 秒；随后 `status.sh` 为 `State=missing`，未启动 Cargo。
- 两个 unit 均无 `ExecMainStatus`、`CommandExit`、测试执行数；没有把 0-run 或 apparatus 不可用标为通过。
- PR 创建后一次性检查：base/head 与上述 SHA 一致；GitHub `statusCheckRollup` 当时为空，未伪称 CI 通过。

## 交付状态

代码已提交并 push，独立 PR 已创建；远程 Rust 验证受 grok-bot-tests sync apparatus 超时阻塞，待恢复后由授权方按新单次 unit 继续验证。
