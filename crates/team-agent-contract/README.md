# Team Agent Contract — K1

独立的、仅依赖 Rust 标准库的契约 library。此包是自己的 nested Cargo workspace；不加入旧 workspace，不依赖旧 `team-agent` crate，不注册任何 provider，也不提供 executable。

**当前不是 Kiro 适配器或可运行 Team。** 没有 Kiro profile、默认 provider、Host executor、store、MCP server、CLI、真实 Fork 事务或原生验收。测试中的 `fixture` 不是 Kiro。

设计输入：黄金 [`provider-contract-spec.md`](../../docs/reference/provider-contract-spec.md)，以及 Kiro 独立框架任务书的 K1 边界。旧六家、根 manifest/lockfile 与所有既有 tracked 文件必须保持不变。

## 公共边界

所有 API 在 `team_agent_contract::contract`，不重新包装成巨型 Provider trait。

| 模块 | 职责 |
|---|---|
| `descriptor` | 必填的 15 facets；显式 `Support`；精确 provider/alias 查找；按 operation 校验 required hook 与非法组合 |
| `hooks` | 仅七个 provider 行为接口：H1 `CatalogHook`、H2 `PlanHook`、H3 `MaterializeHook`、H4 `SessionHook`、H5 `SemanticReader`、H6 `InteractionHook`、H7 `ForkHook` |
| `plan` | `resolve_launch`、不可直接构造的 `ResolvedLaunch`；结构化 argv/env/资源请求/载体报告；`validate_launch_plan` |
| `delivery` | profile、确认/重试/queue/timing 的有限数据；`resolve_submit_policy`；H6 帧上下文；typed message/attempt、物理步骤与 effect 收据 |
| `probe` | `Probe<E>`、相互独立的 T1/T2/T3 evidence；使用显式时钟与身份 fence 的纯 `evaluate_send_gate` |
| `session` | 本席捕获的 local `ResumeBinding`；精确 provider/native identity/cwd/source generation/backing 验证 |
| `fork` | 三种独立 Fork mode、`resolve_fork`、中间计划与 F0–F5/补偿收据；`ContextClone` 不属于 Fork mode |
| `types` | typed IDs、支持状态、错误、身份、证据引用与受限路径请求 |

15 facets 是 `identity/model/effort/auth/bypass/prompt/mcp/tool_names/session/fork/input/startup/probe_sources/workspace/teardown`。descriptor 不持有 executable callback。`ProviderHooks` 恰好七个绑定，无 `Default` 或空成功实现。`BoundedReadHost`、`OwnedIo` 是框架执行端口，`DeliveryObserver` 是事件消费者，不是额外 provider hooks。

### 调用顺序

1. `resolve_provider` 明确 canonical-only 或显式 aliases；没有 unknown → generic fallback。
2. `resolve_launch` 校验 descriptor/bindings、绝对路径、catalog/native identity、model/effort、auth/bypass、完整 worker 的 prompt/MCP/profile，以及 fresh/resume 的互斥。
3. H2 `PlanHook::plan(&ResolvedLaunch)` 只产生计划；随后必须调用 `validate_launch_plan`。H2 不拥有时钟、ID 分配、物化、shell quoting 或 spawn。
4. 后续生命周期再把 owned requests 交 H3/框架端口。`Specified` 只证明计划报告了载体位置，不等于 Materialized 或 NativeConsumed。
5. 输入前用 `resolve_submit_policy` 固定 operation/channel/native/profile/证据引用，使用当前 probes 判定业务输入资格。Native control/startup 使用独立 operation guard，但必须由 K2 的同一物理 executor 执行。terminal policy 不接受 ACP/direct stdin；RPC 必须是独立的零按键路径，当前包未实现。
6. Fork 的 F0 先 `resolve_fork`；新席把 sealed `ResolvedFork` 传给普通 `resolve_launch` / H2，**在 H7 写任何文件前**检查目标 model/effort/MCP 和 launch plan。H7 只返回 backing/intent，不包含 `LaunchPlan` 或 argv；其结果须通过 `validate_native_fork_plan`，随后 K3 才可 register/spawn。validator 失败不能抹掉已发生的 staging effects，调用方须保留 plan/owned-I/O journal。

Full snapshot 的目标 SID/path 由框架预分配。`ExpectedSession::SnapshotTarget` 只是预期，不伪造捕获过的 ResumeBinding；H7 返回的 ExactBacking binding、owner/generation、operation、独占 backing receipt/hash 必须一致。新席首次启动使用已分配的 target generation，不冒充 restart；普通 `Resume` 仍要求比 source 更晚的 generation。`NativeNewSeat` 预期捕获不同于 parent 的 child SID，不等于恢复 parent；InWindowBranch 不走 launch 路径。

`FullWorker` 不能静默降成 `LaunchOnly`。模型 ID 原样保留，ExactCatalogId 只做精确匹配；opaque ID 只能在 descriptor 明确声明时使用。effort 的统一语法只允许六个小写值，按黄金规范允许外围 whitespace trim，不降档或改写未知值。team 默认值只有 `inherit_team_default=true` 才继承；Ignore 必须有理由并反映到 carrier report。

`LaunchPlan` 使用 `OsString` 参数边界，不持有 shell 命令。环境 delta 先 unset 再 overlay，不读取真实环境。载体报告覆盖 model/prompt/MCP/bypass/effort；validator 检查引用、请求与载体一致性、flag effort 值、session 和资源归属。**原生 flags、配置编码和文件被 native 消费的真实性仍须适配器测试/原生证据，结构检查不能替代。** 含 prompt/env/config bytes 的计划类型不提供自动 Debug dump。

### 物理/证据安全边界

- 每个 policy 只有一个 initial logical Enter。LF、Ctrl+J 是不同键，不能冒充 submit。所有额外动作有 guard、有限上界与显式 timing；confirmation 和 generic retry 互斥。
- `key_upper_bound = 1 + C + R + W + ΣQ`，使用 checked arithmetic；同时受全局 budget 约束。startup ack 有独立 step 分类。这里不发送任何键，不声称实际计数。
- H6 只解释传入的 frame/baseline/current typed identity/paste latch，不保存全局 attempt 状态。业务正文里的 token 不能重新定义 message ID。
- `DeliveryEffect::retain_floor` 单调保留已达 effect。未来 executor/journal 必须实际应用它：post-key timeout、observer/persistence failure 不能降成 NoEffect 或重贴。
- Probe 的 Observed/Pending/Negative/Unknown/Unsupported/TimedOut/Error 保留区别；没有由一个层级隐式转换成另一层的 API。
- 业务 gate 要求当前 T1、确切 composer、当前 tool binding；K2 的组合 startup gate 还须应用 profile 所需 server/startup 门与当前反证，不能丢弃未传给此函数的分层事实。显式的一次 FirstBusiness bootstrap 只豁免 Pending/Unknown binding；不能绕过 process/composer/freshness 失败。返回 `consume_bootstrap` 是状态持久化方的责任，不会在纯函数里消费或自动续发。
- T3c 不参与首条消息输入 gate，避免等待自己的回复。server invocation、response write、client consumption、caller presentation 是不同 facts，不能以排序自动推导。
- `CapabilityEvidence` 是外部证据的 scoped reference，不是这个 library 签发的真实性证明。policy resolver 检查版本/engine/UI/OS/binary/profile/candidate/operation/channel/evidence kind 的一致性；它不读取证据文件，也不能把 fixture 变成 native 验收。

### 后续 owner 的硬边界

K2 实现真正有界的 Host/IO/唯一 executor、采样和 journal；K3 实现 store/lifecycle/MCP/Fork 事务；K4 才能添加经 R0 验证的 Kiro descriptor/hooks/profile。K1 的 port/receipt 类型不是这些实现的替代品。

`OwnedPath` 只有 lexical 约束，不证明没有 symlink、并发 writer 或复用 PID。实际端口必须按自身 captured grant 核 root/owner/generation、no-follow/exclusive-create、输出/deadline 和 partial effects；不能把传入对象视作权限。常规物化不允许 global settings/native database。清场还需 fresh ownership/CAS/共享资源判断，不能仅凭 `OwnedRemovable` 删除。

## 验证

Cargo/rustc/rustfmt/clippy 仅在 Grok Bot 或授权 CI 执行（Rust 1.95）。首次在该环境生成并提交本包 lockfile；后续使用 `--locked`：

```sh
cargo test --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets
cargo clippy --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path crates/team-agent-contract/Cargo.toml --check
```

`tests/contracts.rs` 是开发自有的公开纯行为测试，不是隐藏红测。新 crate 在黄金基线上不存在：baseline 标 N/A，不能把找不到 crate/符号的编译失败写成行为 RED。真实运行数、退出码、source/tree 和 byte-freeze 证明随 PR 构建收据记录；本文不预宣称通过。
