# K3：共享生命周期与 MCP 事务——实现及设计报告

## 范围与结构

独立基于 K1 冻结基线；不依赖 K2 分支、不注册 Kiro、不接管旧六家、不修改旧 workspace、黄金规范、技能或配置。实现落在 `orchestration/{store,lifecycle,mcp,supervisor}.rs`，而不是 adapter 的私有队列。只有原来的七个 Provider hooks；新增的 `LifecycleHost` / `DeliveryHost` 是框架执行端口。

- **store**：独占私有 root 的 SQLite DB，scope/endpoint/root 身份校验；seat、不可复用的 instance/generation、operation 与 lease、消息/result/outbox、幂等 call 和独立协议 facts。
- **lifecycle**：纯解析/计划 → owned 物化 → 注册 → spawn → session capture → commit/receipt；restart、三类 Fork 与 teardown 共用该事务。
- **mcp**：只开放 `send_message`、`report_result`、`get_team_status`；另提供有界 newline JSON-RPC stdio loop。没有第四个 lifecycle 工具。
- **supervisor**：对统一 durable outbox 做原子 claim，消费一次性 bootstrap，再交唯一 physical executor；不拥有键序、timing 或 adapter queue。

## 为什么使用短 SQLite 事务 + 持久 operation lease

不能把跨文件/进程/native 控制包成一个虚构 ACID transaction。SQLite `BEGIN IMMEDIATE` 只覆盖短的状态变更；跨外部动作期间使用持久 seat lease。所有副作用前写 `pending`，外部动作后补实际 receipt，失败/重启时保留不确定性而不是重复执行。

`operation_id`、scope/seat/instance/generation、endpoint、process birth identity 与 native SID 是不同的身份，不能互换。begin 同时锁 source/target，并比较读取的原记录；新 generation 必须严格接续**已分配的最大 generation**，失败分配也不复用。停止旧 generation 后才允许新 generation 注册；旧 MCP context 与旧 queue target 不能自动迁移到新 generation。

K1 的少量持久类型增加 serde 支持；typed IDs / `OwnedPath` 反序列化仍走其验证构造器。没有给 descriptor、hook 或 capability 增加隐含默认值。

## F0–F5、故障与补偿

| 窗口 | 行为 / 恢复规则 |
|---|---|
| F0 preflight | `resolve_launch` / `resolve_fork`、H2 与 launch validator；未知/unsupported、model/effort/resume 冲突在任何 stage/stop/spawn 前拒绝。 |
| F1 stage | 拦截 H3/H7 实际 `OwnedIo` 写入，先记 `MayHaveWritten`，再保存完整 receipt；hook 返回空 partial receipt 不能抹掉 journal。H3 只能写 plan 中完整相同的 request；InWindow/NativeNewSeat H7 不得物化文件。 |
| F2 register | 只更新本 target；租约 + 注册 + operation 更新原子化。失败后 CAS 回退本操作注册，不覆盖 parent/sibling 的状态。 |
| F3 spawn | 先持久化 intent；收到合法 PID/birth/executable/hash 后才记 process receipt。没有 receipt 的 spawn 错误保持 `NeedsRecovery`，不能当作进程不存在。 |
| F4 commit | 保存确切 session binding 与 Starting 状态；不是 T2/T3b ready。resume 必须捕获同 SID，snapshot 必须目标 SID/backing，native child 必须不同于 parent。 |
| F5 receipt | 与 lease release 同事务。F4 已成功而 F5 丢失时，恢复只补收据，不停成功进程、不重发 native branch。 |

InWindowBranch 只有 intent/control/capture 路径，**没有伪造 F2/F3 新席阶段**；保留 parent binding，当前 SID 必须变化，旧 readiness 失效。控制一旦发出，后续失败保持 unknown/lease，不再次发送。snapshot 的 H2 preflight 在 H7 写入之前；H7 backing receipt 必须经过 K1 validator，并与拦截 journal 一致。

补偿与清场分开：已知的本次目标进程由 Host 重新核身份后停止；可删除资源必须满足 owner/generation、root、exclusive、确切 bytes hash、OwnedRemovable，且 Host 在删除瞬间复核没有 writer/共享引用或内容变化。session backing、shared/global/native DB、不确定写入一律保留。失败新席解除自己的注册；失败 restart 恢复的是**已停止的 parent**，不能假装旧进程仍 ready。restart 的旧 generation 文件显式列为 preserved，不将旧 receipt 当新 generation 删除权限。

`recover` 不自动发现/领养未知进程，不自动解封 unknown spawn/control 或 uncertain delivery。此类对象保留持久 lease 与现场，由后续带新鲜证据的 operator reconciliation 处理；不能直接删 lease 凑恢复成功。

## MCP 与队列事务

`CallContext` 由已绑定的 server/transport 注入，包含确切 instance/generation、binding key、task 与每条连接唯一的 connection ID。arguments 不能伪造 sender/agent/task；同一连接 RPC ID + 同一 request 返回已保存 response，重用 ID 改 request 拒绝。连接间 RPC ID 可重用。report 额外按 scope/task/agent 保持内容幂等。

- send 的消息行、outbox 和 call response 一次提交；`mailbox=true` 只 durable、不建 live outbox。广播同一事务，不能半个广播成功。
- report 的 result、可选 leader notification outbox、幂等 response 一次提交；silent/casefile 不要求 leader pane。没有 leader 时保留未绑定的通知，不撤销 durable result。
- result persisted 不等于 leader_notified；仅写出且 flush 后记录 `ResponseWritten`。本实现不制造 client consumption / caller presentation。
- 单 outbox 服务所有 provider。已解析目标固定 instance/generation；只有本来没有 leader 的通知允许之后绑定当前同 scope leader。
- claim、bootstrap 消费、target lease 与 executor intent 同事务。整个 executor 调用可能已发 Enter，因此崩溃窗口保守记录 `MayHaveSubmitted`，不能降成 NoEffect。
- receipt 严格比对 identity/message/attempt/operation/channel/policy。任何未知、错绑或 persistence 失败保留 effect floor，隔离 target，不重新 paste、不自动 flush、不把物理提交写成业务 delivered。

### 协议复用取舍

旧 `MessageStore` / MCP `wire` helper 与旧 runtime/types 耦合；直接链接会违反本包与旧 crate 隔离、旧文件零改动的边界。因此保留 messages/results 的共享数据语义，在新 DB 内用同一 SQL transaction 实现原子变更，**不打开或迁移旧 `team.db`**。这里只保留新运行时所需字段，不宣称旧 DB schema/迁移兼容。

生产三工具 schema 是旧 `tools_contract()` 的冻结 JSON 快照；测试 oracle 抽取旧纯 helper，逐 token 验证原文件未漂移，再比较生成 JSON 完全相等。oracle 仅在测试编译，不引入旧 handler。这是避免第四套“自行设计 schema”的兼容边界，不冒称已完成跨 crate 提取。新 runtime 对 task override 的权限更严格：只能等于绑定 task；serverInfo 使用自己的 runtime 名称。

## K2 / K4 接线义务与有限边界

1. 入口 owner 创建**新、独占、私有** runtime root 与 tmux endpoint，构造 descriptor/hooks、已核 native profile、catalog、`LaunchRequest` 和 route；不要采用旧 workspace。
2. `LifecycleHost` 的 spawn/stop/session/cleanup 绑定 K2 owned Host；stop 按 birth identity/endpoint 复核，未知拒绝。H3/H7 的 `OwnedIo` 必须核 no-follow/exclusive grant、稳定完整源读取、snapshot 格式/header/链与并发 writer，不因传入 `OwnedPath` 就授予访问。
3. native control 与 `DeliveryHost` 只接 K2 唯一 physical executor；executor 在**实际动作前**重新核当前 probes/operation guards、profile/evidence、时序和 budget。该 port 不是可绕过这些门的 raw send-keys API。
4. readiness 传入独立 T1/T2/T3a/T3b；server handshake 不推导 client binding，更不推导 T3c。首次 bootstrap 只豁免允许的一次 Pending/Unknown binding；当前反证、错 PID、过期、错误 composer 仍阻塞。
5. 生产 transport 创建唯一 connection ID，防止不同 MCP 连接的 RPC ID 碰撞；bounded stdio 不承担网络认证，不能把任意客户端传来的 context 当授权。

没有在本 PR 中实现 native Kiro profile、可运行 CLI、真实 tmux/K2 组合、订阅验收或生命周期 unknown 现场的自动 reconciliation。SQLite 私有 root 可防误接管和 symlink；不承诺防御具有同 UID、可替换所有 DB bytes 的恶意并发写者。Host 的真实进程/文件归属门不能被 fake tests 替代。

## 验证策略

开发自有 tests 使用真实 SQLite + 可控 fake H2/H3/H4/H6/H7 和框架 Host。覆盖 startup/reopen、resume 世代隔离、三 Fork、unsupported 零 effect、partial write、未知 spawn、F2/F4/F5 SQL 故障、原子 report/outbox 回滚、并发 call 幂等、一次性 bootstrap、post-submit 持久化失败不重投、teardown backing 保留、协议快照相等与非法序列化。

测试/格式化/clippy 在 Grok Bot Rust 1.95 执行；原始退出码与数量以 PR 交付收据为准。fake PASS 不升级成 Native evidence，不宣称完整 Kiro 接入。
