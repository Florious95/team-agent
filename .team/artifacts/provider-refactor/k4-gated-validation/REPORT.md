# PR-K4 gated draft：部分交付与验证收据

**状态：可独立审查的文档候选 adapter + 负向原生门禁；不是 K4 完整原生接入。**

## 版本、分支与证据

- 分支 `feat/kiro-contract-adapter`，基于 K3 `ae6ee58d`（PR #305）；K2 尚未组合。
- 最终 tested source：`af0da943d42eb2a3fef7110ff408125bb7161286`。
- Tested tree：`9d3f5ed02e3f0477d170433e1268f6e951c9d274`。
- 归档提交仅新增本目录，产品与测试 bytes 不变。
- 设计/边界报告：[`docs/kiro-adapter.md`](../../../../crates/team-agent-contract/docs/kiro-adapter.md)。

## 已交付

唯一 KiroDescriptor（`kiro` / `kiro-cli`、15 facets）；H2 文档参数/配置计划、H3 owned materialization/validate/partial receipts；官方 exit-code 分类；12 个 adapter tests。无私有队列、无新 Keystroke/Probe/Teardown adapter hook、无旧六家改动。

生产 native entry 明确关闭：H2 即使经被修改的 fixture descriptor 取得 ResolvedLaunch，遇 `EvidenceKind::Native` 仍拒绝；catalog/session/semantic/interaction/fork 五个 hook slots 为 Unverified，snapshot/native-new-seat F0 Unsupported。没有任何“通用 >”、伪造 JSON schema、固定双 Enter/500ms 或自动重发。

## Grok 真实运行

| Unit | 结果 |
|---|---|
| `gb-kiro-k4-gated-a1` | Exit 101：把不存在的 `Effort::Off` 当成 K1 enum；编译失败，未跑测试。改成 K1 确实存在、但 Kiro 文档无映射的 `Ultra` 拒绝门。 |
| `gb-kiro-k4-gated-a2` | Exit 101：测试空 slice 比较的类型推断错误；改为 `is_empty()`。未跑到测试，不冒称行为 RED。 |
| `gb-kiro-k4-gated-a3` | **ExecMainStatus=CommandExit=0；72 tests PASS、0 FAIL**（K1 34 + Kiro 12 + lifecycle/MCP 16 + transaction faults 10）；fmt `--check`、clippy `-D warnings` 通过。 |

命令为独立 manifest 的 locked `cargo test --all-targets` / clippy / fmt，详见 raw command/journal。Rust/Cargo 1.95.0，Linux Grok Bot；本机 Mac 未执行 Cargo/rustc，没有新建/重试 Kiro 原生进程或读取认证资料。

原始 unit command/status/exit/journal/artifact hashes 保留；skill commit、工具链和 cgroup=applied 均在 journal。三个 unit 经 receipt-bound wipe，并保留 `Wiped=yes` post-wipe 回核与 local-cleanup 收据。a1 cache 只供 a2/a3 顺序复用，完成后已清除。

## 原生输入的真实边界

`research/` 是 leader 指定档案的只读副本：RESEARCH-RECEIPT、native-help timeout 收据、fetch receipts 与七份直接使用的官方文档。七份文档 SHA256/bytes 已逐项匹配原 fetch receipts。不是新采集的 native evidence。

2.28.0 仅来自安装 bundle metadata；五项 version/help 结果全部 15 秒 harness TIMEOUT。没有 native catalog/schema、Luna ID、提示符、键序/时序、SID/resume/Fork、MCP/client 或 exit 实测。这些事实已向 leader 报告，不能因为本轮 fixture PASS 而改为 Supported。

## 阻塞与下一步 owner

1. **R0 缺失**：由 leader 准备固定候选和符合模型约束的 native caller，取得 K01–K11 最小原始包；本轮没有原生启动重试授权。取得前，剩余五个行为 Hook 与 InputProfile 不能诚实实现为原生 Supported。
2. **已证组合边界**：Kiro agent 文件按官方文档在工作 cwd，K3 JournaledIo 目前只允许 runtime-root。两者不同会被安全拒绝。已报告给 leader；需要 lifecycle owner 的最小 scoped WorkingDirectory grant + 正反例验证，不能简单放宽 root 或改变用户 cwd。此 PR 未越界修改 K3。
3. **K2/K3 组合 / public CLI / Mac Luna live acceptance**：NOT-RUN，不在本 gated draft 中承诺。

因此 PR 保持 Draft，不请求合并或宣称完整 Kiro 接入；所有当前可在已有证据下完成的确定性工作已落盘、推仓并测试。未 merge main、tag、release 或 publish。
