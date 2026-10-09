# PR-K3 交付收据

## 身份与范围

- 分支：`feat/kiro-contract-lifecycle-mcp`；独立基于 K1 `2e43ad027a80364c162b5d0d81b406b8d6d566fc`，不包含 K2。
- 最终 tested source：`cb486c88c8c4af521ad25df81e42751fc4d219c3`。
- Tested tree：`a9b778f5144af167aea7372b9e26c9d43f54630a`。
- 后续归档提交只新增本目录证据，产品/测试 bytes 不变。
- 设计报告：[`crates/team-agent-contract/docs/lifecycle-mcp.md`](../../../../crates/team-agent-contract/docs/lifecycle-mcp.md)。
- 交付 shared SQLite scope/instance/generation/lease、F0–F5 lifecycle/restart/fork/teardown、三个 MCP handlers/stdio、通用 outbox supervisor、受控 fake-provider 集成桩。没有 adapter 私有队列。

## 真实验证

| Unit | Source | 结果 |
|---|---|---|
| `gb-kiro-k3-prepare-a1` | `7bcbce57` → format/lock `32ca4d60` | ExecMainStatus=CommandExit=101：两处 SQL iterator borrow 生命周期编译错误；未执行测试，不算行为 RED。 |
| `gb-kiro-k3-check-a2` | `197e43e3` → format `813cd80e` | Exit 0：34 K1 + 16 lifecycle/MCP = 50 tests；clippy `-D warnings` 通过。此结果不是最终新增故障测试的证据。 |
| `gb-kiro-k3-final-a3` | `f2992c67` → format `cb486c88` | ExecMainStatus=CommandExit=0：**34 K1 + 16 lifecycle/MCP + 10 transaction faults = 60 tests，0 failed**；clippy `-D warnings`、fmt `--check` 通过。 |

最终执行：

```text
cargo test --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets
cargo clippy --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path crates/team-agent-contract/Cargo.toml --check
```

全部 Rust 编译/测试/格式化在 Grok Bot（Linux x86_64，rustc/cargo 1.95.0）完成，未在 Mac 执行。lib 的 `running 0 tests` 不计入 PASS 数量；60 是三个 integration binaries 的实际总数。

Final journal 包含最终 source/tree 与旧路径 byte-freeze 的 exit-0 检查；除新 crate 外，K1 基线已跟踪文件零修改。新归档目录不改变旧六家、根 manifests/lockfile、提示词或黄金规范。

各 unit 子目录保留原始 command/status/journal、真实退出、artifact hashes、验证 marker 与清理收据。Grok apparatus skill commit `4a53fbfd366ddbf5f3bfae7a85f9be0382e5d0e1`，helper SHA256 `e32b552e6c40da3d42dc83d16ac40290fed09525a6f6eb413b00b060e96bc264`；cgroup receipt outcome=applied。a1 的 cache 仅顺序供 a2/a3 使用，最终一并清除。

## 清理与保留

- 三个 unit 均 receipt-bound wipe，`post-wipe-status.txt` 证实 `Wiped=yes`；原始 exit 未改写。
- 三个普通 clone scratch 由 `cleanup-local.sh` 按本 unit marker 清除；专属开发 worktree 与分支保留供 PR。
- evidence 归档保留；没有创建真实 Team/native process/tmux 会话或接触订阅认证。

## 限定与未执行

- PASS 是 **shared core + SQLite + fake provider**，不是 Kiro native 支持。
- K2 real Host/executor 组合、真实 tmux/Mac candidate、订阅自然回程、公开 CLI：NOT-RUN / 不在此分支实现。
- 未知 spawn/control 与 uncertain delivery 保持 durable fence，不提供无证据自动 takeover/retry；需要后续 scoped operator reconciliation。
- 旧 wire/schema 复用的隔离取舍、旧 generation 资源保留与 Host ownership 义务均见设计报告；不能把 fake receipt 当真实进程/文件删除授权。
- 未 merge main、tag、release 或 publish。
