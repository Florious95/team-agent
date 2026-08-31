---
name: communication_mode 黑盒验收矩阵
type: requirement
status_at: 0.5.57
domains: F2 F3 F5 F6 F7
---

# `communication_mode` 黑盒验收矩阵

本页是 F2、F5 的跨页黑盒验收锚。0.5.57 尚无双通信契约能力；本页定义新增承诺，不预支在途版本。

消息平面姊妹需求另见 [[F4 消息寻址、受理与可靠送达]]「单信箱参数与 durable-only 回执」、[[F5 Leader 被动收信与呈现]]「上桌、信箱与事实穿透」、[[F6 任务、结果与交付闭环]]「result_route 与流水线结果」。`communication_mode`、send 一比特选择、task 级 `result_route` 是正交维度，彼此不得覆盖。

## 配置求值门

1. omitted/omitted → `leader_centric`。
2. team=`orchestrated`、role omitted → `orchestrated`。
3. team=`orchestrated`、role=`leader_centric` → `leader_centric`。
4. team=`leader_centric`、role=`orchestrated` → `orchestrated`。
5. team 或 role 为未知值 → spawn 前 fail closed，roster/state 无半成员。

## 24 格契约投影门

对以下 2 × 4 × 3 的每一格执行同一组观察：

| 模式 | 生命周期入口 | Provider |
|---|---|---|
| `leader_centric` / `orchestrated` | fresh/quick-start | Claude / Codex / Copilot |
| `leader_centric` / `orchestrated` | fork | Claude / Codex / Copilot |
| `leader_centric` / `orchestrated` | restart/resume | Claude / Codex / Copilot |
| `leader_centric` / `orchestrated` | start-agent reconstruction | Claude / Codex / Copilot |

每格共用判据：

1. 成员自报并可由接收侧核对的 identity 正确。
2. 完成路径只产生一次 `report_result`；零次或多次均失败。
3. 权限段仍存在且语义不变。
4. 人为制造不可继续的阻塞，worker 必须主动报告。
5. 有效契约只能来自两个官方模式，不接受任意 runtime-contract 文本注入。

`leader_centric` 每格追加判据：

1. 默认未声明时与既有通信行为兼容。
2. Progress、blocker、question 的既有指引仍在。
3. 一般 leader/teammate 来信仍产生响应义务。

`orchestrated` 每格追加判据：

1. 未声明 Progress 通道时，不发送 Progress 仍可正常完成。
2. 派单声明指定通道后，过程消息只按该声明发送。
3. 任务相关消息得到响应。
4. 注入纯 ACK、无关状态和非任务消息，不产生被迫回复。
5. blocked 与最终 exact-once 结果义务不因模式改变。

判真口径：

- 只认成员实际收到的契约语义、接收侧消息/结果和落盘审计，不认配置解析器自报“已选择”。
- Provider 间比较共同段与模式差异的语义投影；不把各 provider 的承载差异写成需求。
- 24 格任一格出现 fresh/restart 分裂、默认值漂移、共同段缺失或额外强制通信，即整体不通过。

## 跨功能不变量

- **F3 Provider、模型、权限与资源组合**：Claude、Codex、Copilot 必须获得语义等价的有效契约；比较契约语义，不要求 provider transport 字节相同。
- **F6 任务、结果与交付闭环**：两种模式的 exact-once 最终结果义务完全相同，不能把“不强制 Progress”解释为“不交结果”。
- **F6 任务、结果与交付闭环**：`result_route` 由派单方写在 task 上；`communication_mode` 不改变 route，worker 也不因模式承担路由判断。
- **F7 会话身份、续作、克隆与分叉**：fresh、fork、restart/resume、start-agent reconstruction 的有效模式一致；恢复不得退回默认，fork 不得因新身份丢失模式。

## 非目标与不得外推

1. 不提供第三种用户自定义通信模板。
2. 不开放任意 system prompt/append prompt 替换。
3. 不定义任务工作流、阶段图、审批链或固定角色。
4. 不改变 identity、exact-once、权限、投递与 presentation 契约。
5. 不把 `orchestrated` 解释为“无需回应任何消息”或“可以静默 blocked”。
6. 不把架构盘查中的注入载体、拼装顺序或内部字段写成功能承诺。
