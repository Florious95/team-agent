---
name: F5 Leader 被动收信与呈现
type: requirement
status_at: 0.5.57
architecture: C3 C4 C5 C6
---

# F5：Leader 被动收信与呈现

## 一、用户获得什么

leader 不需要持续查询 inbox、status 或日志，worker 的业务消息与最终结果会主动进入 leader 正在使用的对话主屏。底层 target、transport、提交动作和重试对 leader 隐藏；呈现只携带可读业务内容和确需判断的异常。

用户可见承诺：

1. **无需轮询**：leader idle 或 working 时都能被动收到消息，不靠额外终端 tail、不靠再次启动替代 leader。
2. **进入真实对话**：文本必须成为 leader provider 的真实输入或已确认排队输入，不能只写数据库、日志或屏幕旁路。
3. **人类可读**：默认呈现发送者、任务语境和正文，不把完整控制 JSON 塞进业务上下文。
4. **呈现不改变消息真相**：上屏策略不能把 accepted 冒充 delivered，也不能因静默展示策略消灭用户交付债。
5. **用户交付默认可见、状态事实必穿透**：普通 `report_result` 默认进入 leader 主屏；task 显式声明 pipeline 时才归档而不即时上屏，failed/fallback/blocked/timeout 等状态事实无条件上桌。
6. **通道变化由基础设施承担**：leader target 暂时不可用时，消息先保留交付义务；恢复后继续同一消息，不要求 worker 重报。
7. **过程通信服从所选契约**：`leader_centric` 保持既有过程汇报和响应义务；`orchestrated` 不强制 Progress，任务相关来信按派单声明通道处理。两种模式都不得静默阻塞，且最终结果仍进入既有结果闭环。
8. **上桌与信箱的决定最小化**：普通 send 由发送者在当刻只选择上桌或投信箱；框架只允许状态事实强制穿透，不以发送方自报 stage 类替代事实。

准则锚：

- `docs/team-agent-foundation-and-boundaries.md` §1 第 4、6 条
- 同文 §12「Agent 与 MCP/Runtime 职责边界」
- 同文 §13「控制平面与模型上下文」
- 同文 §14「可靠送达的业务定义」

## 二、怎样黑盒判真

### Idle leader 被动收信

原文锚：

- `docs/team-agent-leader-passive-injection-goal.md` §2：不要求 leader 重启、前台运行或持续轮询
- 同文 §4.3「真实 Codex CLI 验收」：唯一文本作为真实输入进入原 leader 对话并上屏
- 同文 §4.4「非验收路径」：只写 inbox log、tail、collect 后才看见、重启 leader、只注入未提交均不算完成

黑盒验收：

1. 使用已经运行的 leader，不新建替代会话。
2. leader 保持 idle，且不执行 collect/status/inbox/tail。
3. worker 发送唯一文本；该文本自动进入 leader 当前对话。
4. 持久行、物理 attempt 与 leader 对话 receipt 指向同一 `message_id`。

### Working leader 不打断、不丢失

原文锚：

- `docs/team-agent-foundation-and-boundaries.md` §14：leader busy 时消息可进入 follow-up queue，但后续必须成为真实用户消息
- `docs/team-agent-leader-passive-injection-goal.md` §3.4：working 状态需使用不会打断当前 turn 的提交策略
- `docs/team-agent-quick-start-and-blackbox-goal.md` §9：worker 完成后消息自然上屏，全程 leader 不读源码、不手工刷新

黑盒验收：

1. leader 正在处理一个长 turn 时发送唯一消息。
2. 当前 turn 不被中断，消息进入 provider 的后续输入边界。
3. 当前 turn 结束后，该消息恰好一次进入 leader 对话。
4. 不能因输入框外观、placeholder 或“看起来 busy/idle”推断成功或失败。

### 人类可读呈现与事实穿透

原文锚：

- `docs/team-agent-foundation-and-boundaries.md` §13：默认上屏格式为 `Team Agent message from ...`，raw JSON 只用于存储、事件和 debug
- `docs/team-agent-codex-foundation-rust-goal.md` §9：leader 不轮询即可收到人类可读消息
- `.team/artifacts/0.5.60-message-plane-requirements-20260725.md` §2：默认 result 为用户交付；pipeline route 可归档；状态事实无条件穿透

黑盒验收：

1. 普通 message、普通 report_result、投信箱消息、pipeline result、failed/fallback/blocked/timeout 各产生一次。
2. 普通 message/result 默认在 leader 主屏可读；投信箱消息和 pipeline result 按各自声明持久化；四类状态事实无论静默声明都进入主屏。
3. 每条 canonical 内容保持不变；呈现策略只决定去向，不改写正文。
4. stored-only 消息/结果可按 case 拉取，但不得自称 notified/delivered。

### 通道缺失与恢复

原文锚：

- `docs/team-agent-leader-passive-injection-goal.md` §3.6：leader 未 attach 或 pane 消失时不得静默声称完整成功
- `docs/team-agent-foundation-and-boundaries.md` §12：target、重试与 transport 由 runtime 负责
- `wiki/contracts/信箱重放契约.md`：恢复保持同一消息，只有合法 blocked leader row 可重开

黑盒验收：

1. leader 通道缺失时发送，必须留下稳定 obligation 和明确未送达状态。
2. 显式恢复 leader 通道后，同一消息自动继续，不要求 worker 重报。
3. leader 只收到真正未提交的消息；已提交待 receipt、terminal、非 leader 消息不得重播。

### 两种官方通信契约

跨页验收锚：[[communication_mode 黑盒验收矩阵]]

黑盒验收：

1. `leader_centric` 下，既有 Progress/blocker/question 指引和一般来信响应义务保持兼容。
2. `orchestrated` 下，未在派单声明的 Progress 不因缺席而触发违约；声明了通道的过程消息只走该通道。
3. `orchestrated` 下，任务相关来信必须响应；纯 ACK、无关状态或非任务消息不强迫产生回复。
4. 任一模式发生 blocked 时都必须主动报告，不得静默等待。
5. 任一模式完成时都必须调用 `report_result` exactly once，普通最终结果的可见性和 typed presentation policy 不变。
6. 通信模式只改变模型收到的通信契约，不改变 canonical 消息、投递义务、receipt 或恢复语义。

### 上桌、信箱与事实穿透

正式需求锚：`.team/artifacts/0.5.60-message-plane-requirements-20260725.md` §1、§2。

关联验收：[[communication_mode 黑盒验收矩阵]]。

专项需求与锚号：[[send 单信箱与状态事实穿透]]。

黑盒验收：

1. 普通 send 选择上桌时进入 leader 真实对话；选择投信箱时只持久化并可回溯，不发生 live inject。
2. 投信箱回执必须同时证明 `stored_only` 与 `durable_without_live_inject`，不能把“没有上屏”与“没有受理”混为一谈。
3. failed、fallback、blocked、timeout 等状态事实无论发送方选择什么都必须上桌。
4. stage_pass、final_review 等自报词不能单独触发强制上桌；框架不解释自定义 status 的业务含义。
5. `leader_centric` 与 `orchestrated` 都使用同一一比特 send 选择和事实穿透边界；通信模式不再暗含另一套 message-class。

## 三、现在处于什么状态

**0.5.57 时点：部分。**

发布证据：

- passive injection goal 的核心真实验收已经成为后续整包发布的常规能力：原 leader 无轮询收取 worker 消息。
- `.team/TASKS.md` §4.11「0.5.57 发版闭环」门⑤完成三个订阅场景串行验收，门⑧ npm 升级后新 coordinator 正常工作。
- 0.5.54 的消息定制化呈现已区分 leader/casefile/silent，且 critical class 保持主屏可见；0.5.57 继续守住该呈现面。

状态边界：

- 主动上屏、人类可读呈现和 0.5.54 typed presentation policy 已成为已发布能力；后者是本轮需求收缩/搬家的已发布对照，不再作为目标契约。
- `communication_mode` 是 0.5.57 之后新增承诺；双契约可选择且端到端生效尚未实现，因此本页按新增未实现承诺诚实标为「部分」。
- send 单信箱参数与状态事实穿透是 0.5.60 计划态；0.5.57 的八值 class/policy 仍未完成收缩。
- F4 中仍存在的特定 blocked mailbox 恢复债，不把“leader 可被动收信”整体降为未实现；该债会影响某些断线恢复场景，必须在 F4 保持 partial。
- 不预支 0.5.58。

对应架构：

- [[实时通道解析]]、[[所有权与租约]]（C3）
- [[离线信箱义务]]（C4）
- [[投递执行]]、[[模型提交行为]]（C5）
- [[回执判定]]、[[恢复与调度]]、[[结果与呈现]]（C6）

## 输入从哪个功能来

- [[F4 消息寻址、受理与可靠送达]]提供 canonical leader-bound 消息、交付义务与 receipt。
- F6 任务、结果与交付闭环提供普通结果、内部阶段产物与 critical result。

## 输出到哪个功能去

- 向 F6 任务、结果与交付闭环提供用户可见结果出口。
- 向 F8 状态观察、诊断与人工干预提供 leader channel 与呈现状态。

## 需求已演化待换代

1. passive injection goal 固定 `TEAM_AGENT_MESSAGE <json>` 作为 leader payload；后续基础边界 §13 已要求默认人类可读、raw JSON 仅进控制平面。旧 payload 验收需换代。
2. passive injection goal 固定 tmux pane、Enter/Tab 与进程名校验；现行需求是 transport/provider-neutral 的通道与提交原语。旧 goal 应保留场景意图，删除实现手段的规范地位。
3. quick-start goal 的 stuck timer 主动推送与状态枚举属于早期 coordinator 设想；是否继续作为用户可见承诺需结合“框架不基于弱状态主动 nag”的后续准则单独会签，列入 [[待用户会签清单]]。
4. 0.5.54 typed stage 类曾在 send/presentation 平面表达流水线状态；0.5.60 正式需求将其搬到 result_route + status 平面。旧 stage 类仅保留迁移史地位，落地后的兼容边界列入 [[待用户会签清单]]。
5. 本页「进入真实对话」「无需轮询」依赖 F4 把消息推进到接收方对话。S-004 尚未裁定 delivered 是否换代为 provider 对话 receipt；2026-08-18 派单给定实测里一半以上消息停在 `injected_awaiting_receipt`。原文保留，见 [[待用户会签清单]] S-004。不把停放写成新承诺。
