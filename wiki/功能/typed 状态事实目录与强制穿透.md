---
name: typed 状态事实目录与强制穿透
type: requirement
status_at: 0.5.60
target_version: 0.5.61
domains: F4 F5 F6 F8 F10
---

# typed 状态事实目录与强制穿透

本页是消息面与 result envelope 面共同的状态事实 SSOT。[[send 单信箱与状态事实穿透]] 消费消息面四值；[[result_route 与流水线结果消费]] 消费终报 envelope 三值。两面不得各维护一份手抄目录。

唯一需求源：`.team/artifacts/0.5.60-message-plane-requirements-20260725.md` §2②、§2④、§2b；目录归属补充裁定：leader 2026-07-26。

## 一、用户获得什么

### 行为承诺

| 锚号 | 承诺 |
|---|---|
| B01 | 框架只有一套公开、可枚举的 typed 状态事实目录；消息 policy、result envelope、schema/help 和测试都消费该目录，不手抄第二份。 |
| B02 | 目录固定表达四种事实：failed、fallback、blocked、timeout；每个条目同时声明合法产生者、可消费平面和用户可见语义。 |
| B03 | 消息面看到合法 typed failed/fallback/blocked/timeout 时，无条件上桌，不受 send 投信箱选择、业务 status 或旧 message-class 覆盖。 |
| B04 | result envelope 终报面只接受 typed failed/fallback/timeout；三值无条件穿透 leader，并保留 durable result/失败引用。 |
| B05 | blocked 不进入 result envelope 终报 outcome：它是任务仍可续作时的中态求助，要求 leader/外部条件介入；把它写成终报会同时伪造“任务已结束”和“仍等待解锁”。 |
| B06 | typed 事实只能由框架可验证的状态转换或受约束 typed 入口产生；正文、summary、业务 status、自报 stage/class 或大小写相似词都不能生成事实。 |
| B07 | 业务 status 仍是不透明字符串，由任务词表和编排脚本解释；即使字面为 `"failed"`、`"blocked"` 或 `"timeout"`，也不触发框架穿透。 |
| B08 | stage_pass、final_review、bounce 等流水线自报类不属于事实目录，不再构成强制路由；它们按 result_route 与任务词表消费。 |
| B09 | 每次事实产生、policy 消费、穿透和拒绝都留下可关联 task/message/result 的 typed 审计；不能只在呈现层改路由而丢失事实来源。 |
| B10 | worker 通过 task-scoped 的受约束 typed blocked 中态转换原语发起求助；对 worker 宜表现为独立 MCP 工具。该原语原子保留 assignee、上下文与续作义务，产生目录中的 blocked 事实并强制上桌，但不生成终报 result。 |
| B11 | blocked 的解除权归当前任务的派单方/leader。公开动作复用既有 task-scoped follow-up/再派单入口，但必须显式表达 resume 并引用当前 blocked fact；普通消息不隐式解除，其他发信者无权解除。解除后同一 task 回到可执行态，assignee 与 exactly-once 终报义务不变，原 blocked/resume 因果链永久留在 B09 审计中。 |

### 单一目录、两面消费

| typed 事实 | 合法事实语义 | 消息面强制上桌 | result envelope 终报 | 产生者边界 |
|---|---|---:|---:|---|
| `failed` | 本次任务/结果已确定失败，无法按成功路径完成 | 是 | 是 | runtime 可验证失败；或 worker 通过受约束终报 outcome 提交并由 runtime 校验任务终态 |
| `fallback` | 正常路径失败后已进入明确降级终态 | 是 | 是 | runtime 的 typed 降级决策；或受约束终报 outcome，必须带失败/降级引用 |
| `blocked` | 任务尚未终结，正在等待 leader、授权或外部条件 | 是 | **否** | task 状态机的 typed 中态转换，必须保留 assignee 与续作义务 |
| `timeout` | 已达到框架可证明的 deadline/budget 终止条件 | 是 | 是 | runtime 计时/预算事实；或受约束终报 outcome 引用同一 deadline/budget |

“worker 通过受约束终报 outcome 提交”不等于信任 worker 自由文本：字段值来自共享目录的 envelope 子集，runtime 同时验证任务归属、终态资格和必要引用。worker 不能靠业务 status 或 summary 创造 typed 事实。

### 为什么 blocked 不进入终报

blocked 的用户动作是“介入并让同一任务继续”，不是“消费一个已结束结果”。它必须：

1. 保留当前任务、assignee、上下文和恢复义务。
2. 上桌说明缺什么，不生成完成结果。
3. 解锁后从同一任务继续，最终再 exactly-once 终报。

若允许 `report_result(outcome=blocked)`，框架会把中态求助压成终态结果，后续既可能重复终报，也可能失去续作所有权。因此 envelope 子集必须从共享四值目录按资格投影，而不是复制一份“三值常量”。

## 二、怎样黑盒判真

### 目录 SSOT 与公开枚举

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C01 | B01/B02 | 从公开目录枚举全部事实及消费资格 | 恰有 failed/fallback/blocked/timeout；send 四值=true，envelope 仅 blocked=false |
| C02 | B01 | schema/help/policy/envelope validator/test fixture 分别读取目录 | 新增 synthetic 目录项的测试 canary 能让所有消费者同时看见；不存在手抄集合漂移 |
| C03 | B01/B02 | 提交未知 typed fact | 在污染 task/message/result 前 fail closed，错误给字段路径与合法目录 |
| C04 | B09 | 对四值各产生一次，再产生一次拒绝 | 审计能按同一 task/message/result 引用还原 producer、fact、consumer、route 和拒绝原因 |

测试不得把四值或 envelope 三值手抄进第二份断言常量；应从公开目录枚举，再按每项声明的面向资格生成矩阵。

### 消息面四值

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C05 | B03 | send 请求投信箱，分别产生 typed failed/fallback/blocked/timeout | 四值均恰一次上桌且持久，mailbox 选择不生效 |
| C06 | B03/B09 | 对同一事实重复 policy/recovery/tick | leader 最多看到一次；原 message/fact identity 不变 |
| C07 | B06/B07 | 正文、summary、业务 status 分别写四个同名词 | 仍按原一比特路由，不生成 typed fact、不强制上桌 |
| C08 | B08 | stage_pass/final_review/bounce 自报类分别请求投信箱 | 不因类名强制上桌；按 send/result 姊妹页的 route 处理 |

### result envelope 三值与 blocked 反面齿

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C09 | B04 | pipeline task 分别终报 typed failed/fallback/timeout | 三值都 durable、保留失败引用并恰一次穿透 leader |
| C10 | B05 | pipeline task 尝试终报 typed blocked | envelope 在终态污染前明确拒绝；task 仍 active/blocked、assignee 与续作义务不丢 |
| C11 | B05/B10/B11 | 先通过 typed blocked 中态原语求助，再由派单方显式 resume，恢复同一 task 并成功终报 | blocked 只产生一次中态上桌，assignee/续作义务始终保留；resume 因果可审计；最终 result exactly once，仍归原 task |
| C12 | B06/B07 | pipeline result 的业务 status 依次为 failed/fallback/blocked/timeout | 四个字符串原样 durable 并送脚本，均不因字面穿透 leader |
| C13 | B04/B09 | typed timeout 同时带业务 status=`retry_next` | typed timeout 决定穿透；业务 status 原样保留但框架不解释 |
| C14 | B10 | worker 对当前 task 调用 typed blocked 中态原语，并带原因/证据引用；另用错误 assignee、旧 task、自由文本模拟 | 合法调用原子写 blocked 事实并上桌，task/result 未终结且 assignee 不变；三类非法输入 fail closed、零状态污染 |
| C15 | B09/B11 | 派单方对当前 blocked fact 走 task-scoped follow-up/再派单并显式 resume；另测普通回复、非派单方、错误 task、过期 fact | 仅合法动作解除 blocked，同一 task/assignee 恢复可执行，审计串起 blocked fact、解除者与 resume 动作；四类非法输入不改状态。恢复后 worker 最终只产生一次 result |

## 三、现在处于什么状态

**0.5.60 已发布时点：未实现本目录契约；0.5.61 计划态。**

已发布对照：

- 现行 policy 已有 stage_pass/bounce/blocking/final_review/timeout 五个 critical class 强制 leader 的行为。
- result envelope 已有结构化接收和 durable 存储基础。

仍未满足：

1. 强制面仍锚发送方 class，而非框架可证明的 typed 状态事实。
2. 消息 policy 与 result envelope 尚无一个公开可枚举、带消费资格的共同事实目录。
3. blocking class 与 blocked 中态、timeout class 与 typed timeout 事实尚未严格分层。
4. stage_pass/final_review 等自报类仍可影响 effective sink。

本页不预支实现；现状距离由 runtime-owner 在目标 `origin/main` 上核验。

## 四、产生者与消费边界

### 合法产生者

1. task/runtime 状态机基于可验证输入写 typed failed/fallback/blocked/timeout。
2. result ingestion 接受 worker envelope 的受约束终报 outcome 子集 failed/fallback/timeout，并校验 task 归属、终态资格与引用。
3. deadline/budget、transport、provider 或 fallback primitive 在其权威边界内产生对应 typed 事实。
4. worker 只能通过 task-scoped typed blocked 中态转换原语请求阻塞；该原语校验当前 assignee、live task 与原因/证据引用后，由 task 状态机产生 blocked。需求建议对 worker 暴露独立 MCP 工具，内部可复用 task transition API；不采用 send typed 参数，以免污染 send 一比特，也不采用 `report_result(outcome=blocked)`，以免伪造终态。工具名与内部函数形态不在需求层固定。

blocked 解除不新增顶层命令：复用派单方已有的 task-scoped follow-up/再派单动作，增加显式 resume 语义并引用 blocked fact。不能把“leader 回了一条消息”当作解除，因为消息可能只是追问、ACK 或无关通知；也不能让 runtime 根据外部条件的弱观察自行猜测已解锁。未来若某类条件具有可验证的 typed satisfaction fact，可另行扩展自动解除资格，但首片只认派单方显式 resume。

### 非法产生者

1. worker 正文、summary 或自由业务 status。
2. 旧 message-class、stage_pass/final_review/bounce/blocking/timeout 字符串本身。
3. presentation policy 通过内容 regex 或弱状态猜测。
4. 编排脚本把自己的轮次/分支状态反写成框架事实。

### 消费者

- send policy 只读目录的 `message_force_leader` 资格，不改事实。
- result envelope validator 只读目录的 `terminal_outcome` 资格，不维护三值副本。
- presentation 只渲染已决策目的地和事实引用，不重新分类。
- 测试从目录生成矩阵，不手抄枚举。

## 五、兼容与迁移

| 锚号 | 建议 |
|---|---|
| M01 | 新目录与 producer/consumer 资格先落地，再切换 policy；禁止先删旧 class 后让事实穿透出现空窗。 |
| M02 | 兼容期可接受旧 class 字段，但它不再有事实生产权；只有同时存在合法 typed 事实时才触发穿透。 |
| M03 | `blocking` 旧 class 不能自动升级成 typed blocked：前者是自报分类，后者必须是保留任务续作义务的状态机中态。 |
| M04 | `timeout` 旧 class 不能单独升级成 typed timeout：必须有关联 deadline/budget 或受约束终报资格；兼容期仅按 [[send 单信箱与状态事实穿透]] M02 保守上桌。 |
| M05 | stage_pass/bounce/final_review 迁往 result 平面；其兼容窗口与旧冻结契约终止点继续由 S-011 会签，不在本页另造时间表。 |
| M06 | 目录成为公开 SSOT 后，删除 policy/envelope/tests 中手抄集合；任何新事实必须先改目录与资格，再由所有消费者自动获得。 |

本页没有新增 S-012：目录四值、envelope 三值及 blocked 排除均已由 leader 明裁；旧 class/stage 兼容选择已在 S-011。没有新的用户选择，不重复挂债。

## 六、form8/form14 重锚

| 旧形态 | 冲突 | 新重锚 |
|---|---|---|
| form8 五 critical class 强制 effective leader | stage_pass/bounce/final_review 是自报流水线类；blocking/timeout 字符串也不是 typed 事实 | 从共享目录枚举 message 面四值，逐值证明合法 typed producer → effective leader；另测五个旧 class 无事实时不具强制权 |
| form14 PASS/BOUNCE/BLOCKING/timeout/final-review 各 exactly once 上屏 | 把自报阶段与真实异常混成同一 critical 集 | 改为 failed/fallback/blocked/timeout 四个 typed 事实各 exactly once 上桌；stage_pass/final_review/bounce 走 result_route，不作为穿透正控 |

验证种系签字要求：

1. form8/form14 不得手抄四值；从公开目录按 `message_force_leader` 资格派生。
2. 同轮增加 envelope 投影反面齿：blocked 不具 `terminal_outcome`，failed/fallback/timeout 具备。
3. 每个强制穿透负向“未留信箱”必须配 durable/上桌正控，避免消息根本未产生的假绿。
4. 旧 class 兼容输入与 typed 事实分开造 fixture，禁止用同一字符串同时冒充 producer 和 consumer 证据。

## 七、与姊妹页的顺序约束

| 锚号 | 约束 |
|---|---|
| T01 | 本页先建立共享 typed 事实目录、producer 资格和两面消费投影；同片提供 task-scoped typed blocked 中态原语，禁止留下无 producer 的 blocked 目录项。 |
| T02 | blocked 原语可端到端产生事实后，send policy 再把强制面从旧 critical class 切到目录的 message 四值；form8/form14 同片重锚。 |
| T03 | result_route 片把 envelope typed 三值接入同一目录；blocked 排除齿必须与三值正控同片。 |
| T04 | 两面均消费 SSOT 且旧 class 无 policy 权后，stage_* 才按 S-011/result_route 顺序完成日落。 |

```text
typed facts SSOT
  ├─ message projection: failed / fallback / blocked / timeout
  └─ terminal envelope projection: failed / fallback / timeout
                                  （blocked 明确排除）
```

## 八、非目标

1. 不让框架解释业务 status 词表。
2. 不把 blocked 改成终报 outcome。
3. 不用新 class 枚举替换旧 critical class。
4. 不在目录中保存编排轮次、分支、站型或死结根因。
5. 不从正文、summary 或弱观察猜 typed 事实。
