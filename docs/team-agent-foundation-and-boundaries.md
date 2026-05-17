# Team Agent 基础设施原点与边界

生成日期：2026-05-14

本文是 Team Agent 后续设计、开发、验收和提示词优化的核心约束文档。后续 goal、skill、runtime、provider adapter、display backend 和文档改动不得违背本文。

## 1. 功能原点

Team Agent 的原点不是让大模型变聪明，也不是预设一套固定团队模板。大模型本身已经足够强，能够通过对话完成需求澄清、角色定义、任务拆解和结果判断。

Team Agent 要做的是基础设施：

1. 让不同 CLI agent 能稳定互相通信。
2. 让不同 provider、模型、订阅制资源和 API 资源能被统一编排。
3. 让 agent 的上下文、会话、产物、状态和交接能被保存、恢复和观测。
4. 让主 leader 的上下文尽量只承载业务决策，不被底层脚本、协议、日志和重试细节污染。
5. 让团队启动和运行具备可见、可操作、可诊断的使用体验。

因此本项目的核心定位是：

```text
跨 CLI、跨订阅/API、可恢复、可运维的 Agent 团队基础设施。
```

不是：

```text
固定角色模板库。
固定任务计划引擎。
Claude Team Agent 的简单复刻。
Web UI 产品。
```

## 2. 不替代模型决策

Team Agent 不应把模型已经能做好的事情写成死流程。

不应固化：

1. 某类任务一定要哪些角色。
2. 某个团队必须按固定计划执行。
3. 某个角色必须使用固定提示词模板。
4. 默认恢复旧会话或默认新开会话。
5. observer、reviewer、QA 等角色必须自动开启。

应提供：

1. 事实：已有角色、会话、上下文占用、上次任务、handoff、provider 状态、额度/成本提示。
2. 工具：校验、编译、启动、通信、恢复、关闭、compact、diagnose。
3. 约束：secret 不泄露、控制平面不污染上下文、投递状态可验证。

leader 根据事实和用户目标做判断，必要时向用户确认。

## 3. 用户可见的一等入口

用户可见的一等入口应是文档和对话，不是机器 manifest。

推荐的用户可见输入包括：

1. 需求分析文档。
2. 团队决策文档。
3. 角色定义文档。
4. provider/profile 选择说明。
5. 交接文档和恢复记录。

`team.spec.yaml` 或未来的 runtime manifest 应被视为编译产物。它是机器执行入口，不应要求用户或 leader 一上来手写长配置。

计划不是长期一等资产。计划会随任务推进变化，可以由 leader 临时生成、调整、废弃。角色定义、团队边界、provider/profile 选择、会话恢复信息更适合作为可复用资产。

## 4. 角色定义边界

角色定义由 leader 和用户通过对话形成。Team Agent 不负责判断角色是否“业务上最优”，只负责让已形成的角色定义可执行。

角色文档应能表达：

1. 名称和职责。
2. provider 和模型选择。
3. 授权模式：订阅制、官方 API、第三方 API compatible。
4. profile/credential 引用。
5. 工具权限。
6. 上下文边界。
7. 输出契约。

角色文档可以由 leader 创建，也可以由用户长期维护。系统只做结构校验和运行时编译。

## 5. Provider、Model 和资源编排

Team Agent 的重要亮点是模型资源编排，而不是简单支持多个 provider。

系统必须支持以下组合：

1. Claude 订阅制作为 leader，Codex 订阅制作为代码实现 worker。
2. Codex 订阅制作为 leader，Claude 订阅制作为分析、文档或评审 worker。
3. Codex/Claude 订阅制与第三方 API compatible 模型混合使用。
4. 低成本模型作为执行 worker，高能力模型作为澄清、编排、审查和纠偏者。
5. observer 角色使用订阅制下富余模型额度，例如低成本代码审查、可维护性检查、提交前检查。

这里的产品价值是：

1. 更充分利用订阅制资源。
2. 让不同模型能力互补，实现 1+1>2。
3. 让主节点优化低成本模型的输入、过程和输出，从而提高低成本模型的实际效果。

provider/model 配置必须区分：

1. CLI 外壳：Codex CLI、Claude Code CLI 等。
2. 授权模式：subscription、official_api、compatible_api。
3. 模型名称。
4. base URL。
5. credential 引用。

secret 不得写入角色文档或 team manifest 明文。系统可以生成空白 profile 文件模板，让用户自行填写 API key。真实 profile 文件必须默认被 `.gitignore` 忽略。runtime 只能报告 secret 是否存在、是否可用，不应把 secret 内容注入 agent 上下文或日志。

## 6. Agent 与 MCP/Runtime 职责边界

Agent 只负责表达语义。

例如 worker 只需要表达：

```text
send_message(to="leader", task_id="...", content="...")
```

worker 不负责：

1. leader pane 是哪个。
2. 需要按 Tab 还是 Enter。
3. 是否需要重新 attach。
4. 是否需要重试。
5. tmux/Ghostty/PTY 是否成功。
6. delivery 状态如何验证。

MCP/runtime 负责可靠投递：

1. 接收语义。
2. 写入 SQLite/event log。
3. 发现真实目标。
4. 校验目标身份。
5. 渲染用户友好的上屏内容。
6. 注入到正确 CLI。
7. 验证是否上屏。
8. 失败时重试或返回结构化错误。

任何鲁棒性逻辑都不应要求 worker 或 leader 在自然语言上下文里手动执行底层协议。

## 7. 控制平面与模型上下文

必须区分控制平面和模型上下文。

控制平面包括：

1. message_id。
2. task_id。
3. sender/recipient。
4. pane/terminal id。
5. delivery status。
6. retry 记录。
7. diagnose 细节。
8. provider/profile 状态。
9. session id 和恢复信息。

这些信息应保存在 SQLite、event log、state 文件和诊断报告中。

模型上下文只应接收必要业务内容。默认不应把完整 JSON 协议直接注入给 leader 或 worker。

推荐 leader 上屏格式：

```text
Team Agent message from backend_developer for task_backend_build:

<content>
```

raw JSON 只用于 DB、event log、debug 模式或测试，不作为默认上屏内容。

## 8. 可靠送达的业务定义

可靠送达不应等同于 tmux 命令返回成功。

至少区分以下状态：

1. `accepted`：MCP/runtime 已接受消息并写入存储。
2. `target_resolved`：runtime 找到了当前可信目标。
3. `injected`：文本已写入目标终端。
4. `visible`：capture 或等价机制验证消息已在目标 CLI 上屏或排队。
5. `consumed`：目标 agent 已处理该消息。这个状态需要 ack 或可识别行为确认，不作为第一阶段硬承诺。
6. `failed`：明确失败。
7. `ambiguous`：发现多个候选目标，不能自动判断。

短期核心验收应达到 `visible`。不能把 `injected` 直接称为 `delivered`。

## 9. 会话生命周期与恢复

Team Agent 应记录每个 agent 的上下文空间和会话身份，使下一次任务可以选择恢复或新开。

runtime 应提供事实：

1. agent id。
2. provider/model/profile。
3. session id 或 resume id。
4. context usage。
5. last task。
6. status。
7. handoff path。
8. terminal target。

恢复策略不应硬编码。leader 根据事实判断：

1. 直接恢复旧 agent。
2. 基于 handoff 新开 agent。
3. 只恢复部分角色。
4. 向用户确认。

## 10. 可见可操作的显示体验

Team Agent 启动团队时，应尽量提供可见、可操作、具有视觉冲击的体验。

用户期望：

1. 开启 Team Agent 时 worker 窗口自动弹出。
2. 每个 worker 是干净、原生、可滚动、可操作的 CLI 界面。
3. 不要求用户学习 tmux 快捷键在多 window/pane 间切换。
4. 多角色团队可以先实现每个 agent 一个 Ghostty 窗口，后续再实现 tab/多列布局。

tmux 可以继续作为控制层和 headless backend，但不应强迫用户把 tmux UI 当成主要展示层。如果 tmux UI 破坏 Codex/Claude TUI 的渲染、滚动或输入体验，应优先探索 Ghostty 原生窗口、PTY target 或其他 display backend。

## 11. 前期准备脚本化

主 leader 上下文浪费主要来自底层脚本和观察结果，而不是来自角色思考本身。

应将以下多步操作合并为事务命令：

1. provider/profile preflight。
2. role docs 结构校验。
3. manifest 编译。
4. launch dry-run。
5. leader attach。
6. worker readiness 检查。
7. MCP approval 预检。
8. status/diagnose 汇总。

事务命令应返回短摘要，长日志留在文件中。

示例：

```text
team-agent preflight --team .team/current
team-agent start --team .team/current --yes
team-agent wait-ready --timeout 120 --json
```

leader 只需要读结论，不需要把每个底层命令输出塞进上下文。

## 12. 后续模块拆分方向

建议模块边界：

1. Authoring input：读取需求、团队、角色和 profile 文档。
2. Compiler：把用户可读文档编译为 runtime manifest。
3. Provider adapter：处理 Codex/Claude 启动、授权模式、MCP 注入、模型/profile。
4. Terminal controller：处理 tmux、PTY、Ghostty target、注入和 capture。
5. Message bus：处理 SQLite、消息状态、重试、ack。
6. Delivery verifier：验证目标和上屏。
7. Prompt renderer：把结构化消息渲染为人类可读内容。
8. Session lifecycle：记录 resume、handoff、compact、shutdown。
9. Display backend：处理可见窗口、布局、标题和用户操作体验。

Rust 迁移应在模块边界稳定后逐步进行。优先迁移底层稳定模块，例如 terminal controller、message bus、delivery verifier、provider adapter core。角色协商、文档生成、提示词渲染等变化快的部分不应急于 Rust 化。

## 13. 非目标

当前非目标：

1. 预设完整角色模板市场。
2. 替用户决定所有团队结构。
3. 固化一次任务的执行计划。
4. 把所有 provider 组合做成排列组合式 demo。
5. 用 UI 产品替代 CLI-native 工作流。
6. 让 agent 自己在上下文里学习底层运维协议。
7. 把 secret 写进可提交文件或 agent 上下文。

## 14. 后续工作必须遵守

任何后续 goal 文档和实现必须说明：

1. 是否保持 Agent/MCP/runtime 职责边界。
2. 是否避免控制平面污染模型上下文。
3. 是否把用户可读文档和机器 manifest 解耦。
4. 是否保护 secret。
5. 是否保留 provider/model/profile 的可组合性。
6. 是否减少主 leader 上下文消耗。
7. 是否提升或至少不破坏 CLI 可见操作体验。
