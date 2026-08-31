---
name: send 单信箱与状态事实穿透
type: requirement
status_at: 0.5.60
target_version: 0.5.61
domains: F4 F5 F6
---

# send 单信箱与状态事实穿透

本页是 F4/F5 的专项需求页，只定义 send 平面的收缩：发送者只决定“上桌还是投信箱”，框架强制穿透只锚状态事实。`result_route` 的字段和结果消费规则不在本页设计；本页只声明 stage_* 搬家的日落边界与顺序依赖。

result 后片承接页：[[result_route 与流水线结果消费]]。

强制穿透的共同 SSOT：[[typed 状态事实目录与强制穿透]]。

唯一需求源：`.team/artifacts/0.5.60-message-plane-requirements-20260725.md` §1【一】、§2①–③、§2b、§2d。

## 一、用户获得什么

### 行为承诺

| 锚号 | 承诺 |
|---|---|
| B01 | send 只保留一个“投信箱”布尔选择；未选择即上桌。不得以另一组 class、sink 或 policy 参数让 agent 再做分类。 |
| B02 | 投信箱仍形成稳定 `message_id` 和持久记录，leader 可回溯，但不发生 live inject；回执必须诚实区分 durable-only 与已上桌。 |
| B03 | 已发布八值 `message-class` 取消必填并整体退出发送方契约；信箱浏览标签从发件人、`task_id`、`status` 等既有事实派生。 |
| B04 | failed、fallback、blocked、timeout 等状态事实无条件上桌，不受投信箱选择覆盖。 |
| B05 | stage_pass、final_review 等发送方自报标签不再构成强制路由依据；正文或自定义 status 含同名词也不能改变目的地。 |
| B06 | stage_result、stage_pass、bounce、final_review 的流水线语义归位 result 平面；本页落地后 send 侧同名 stage 类日落。搬家不是删功能。 |
| B07 | 一比特选择不改变 F4 的身份、持久受理、单赢家、receipt、恢复、去重和禁止重复注入承诺。 |

### 为什么是“一比特”而不是新分类

参数必须落在信息所在的地方。发送当刻只有 agent 知道这条消息是否值得打断 leader，但这个判断只有“上桌/投信箱”一比特。让发送者再选八值 class，是要求它揣摩全局 policy；这正是本需求明确取消的分类作业。

任务 outcome/status 与 message-class 不同：前者是派单方定义、每任务自带词表、由脚本消费的任务契约字段；后者是所有发送者都要理解的全局分类。允许任务 outcome 不构成恢复 send class 的理由。

分界自检句：

> 这是不是编排脚本的内容？是，就不进框架。

轮次、阈值、分支表、死结根因分类和下一站选择均不属于本页。

## 二、怎样黑盒判真

### 用例锚

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C01 | B01/B02 | 默认 send 一个唯一 token | token 恰一次进入 leader 真实对话；持久行与 `message_id` 存在 |
| C02 | B01/B02 | 同内容改为投信箱 | 持久行存在、leader 可拉取、零 live inject；回执为 `stored_only` 且 verification=`durable_without_live_inject` |
| C03 | B03 | 分别从 CLI 与 MCP 省略旧 class 发信 | 均成功；工具/schema/help 不再把八值 class 作为必填输入 |
| C04 | B03 | 投信箱后浏览/拉取 | 标签只由 sender、`task_id`、`status` 等既有事实生成；发送请求没有标签负担 |
| C05 | B04 | 在请求投信箱时分别产生 failed、fallback、blocked、timeout 事实 | 四类都恰一次上桌且持久；不得因 mailbox 选择消失 |
| C06 | B05 | 发送普通事实，正文/status 分别含 stage_pass、final_review 等词 | 仍按一比特选择路由，自报词不触发强制上桌 |
| C07 | B05 | 使用旧 stage_pass/final_review class 入口 | 落在选定迁移策略：兼容期只读映射并给弃用证据，或硬断明确失败；不得继续作为 policy 权威 |
| C08 | B06 | send 侧尝试 stage_result/stage_pass/bounce/final_review 流水线语义 | send 平面不再持有分支语义；结果平面尚未就绪时必须明确阻塞，不能偷偷回退旧 class |
| C09 | B07 | mailbox 消息经 restart/rebind/recovery | 仍不注入；原 `message_id`、内容、owner 和 attempt 不变 |
| C10 | B01/B04/B05 | 默认/投信箱 × 四事实穿透 × 两个自报 stage 词做差分 | 只有 mailbox 一比特和真实状态事实改变路由；文本、自报 class 与浏览标签均不改变 policy |

负向判据必须带正向 backing：证明“未上桌”时同时证明消息已持久受理；否则“根本未执行”也会假绿。

## 三、现在处于什么状态

**0.5.60 已发布时点：未实现本收缩；0.5.61 计划态。**

已发布可复用能力：

- 投信箱的核心行为已经存在：持久落盘、零 pane 注入，回执可表达 `stored_only` / `durable_without_live_inject`。

仍有三处距离（需求源 §3 在 `origin/main@b0d81f52` 的 leader 亲核）：

1. CLI 仍把 `--message-class` 绑定为八值必填枚举。
2. policy 仍由 stage_pass/bounce/blocking/final_review/timeout 等 critical class 强制 `effective_sink=leader`。
3. `report_result` 仍有 stage_result + case_id 的强制分流；该处归 result_route 后片，不在本片设计。

本轮只验证 `origin/main@ac0e78b1e7877f05433e263e3c180da03521df6b` 提交存在。因验证种系需求基底不读取产品源码，三处实现距离沿用上述权威实证，须由 runtime-owner/leader 在实现开工前对 `ac0e78b1` 重新出具同口径核验；未核前不得宣称任一距离已消失。

## 四、兼容与迁移

### 迁移建议

| 锚号 | 建议 |
|---|---|
| M01 | **采用一个小版本的兼容期，不立即硬断。** 已发布 CLI/脚本可能仍传八值 class；立即硬断会把语义收缩变成无必要的调用面中断。 |
| M02 | 兼容期内旧参数只允许被解析为弃用输入：`message`/`progress` 等不得继续驱动 policy；能无歧义映射“投信箱/上桌”的旧输入映射到一比特并返回 deprecation 证据。 |
| M03 | 不能无歧义映射的 stage_* 不得猜。若 result_route 后片尚未就绪，明确拒绝并指出迁移依赖；若已就绪，只允许迁移工具/适配层导向 result 平面。 |
| M04 | 新 help/schema/官方模板立即只展示一比特参数，兼容入口不再被文档推荐，避免兼容期变成永久双契约。 |
| M05 | 兼容期结束后删除旧八值输入；删除点与旧冻结契约终止点由 [[待用户会签清单]] S-011 裁定。 |

M02 逐值映射（枚举拼写按 msg-routing 冻结契约 form 清单）：

| 旧 `message-class` | 兼容期去向 | 理由 |
|---|---|---|
| `message` | 映射为上桌 | 普通业务消息的既有默认就是 leader 可见；省略新布尔值时同样上桌，零语义迁移。 |
| `progress` | 映射为投信箱 | 过程进度默认持久可回溯但不应打断 leader；新调用者此后须由 agent 直接选择一比特。 |
| `stage_result` | 明确拒绝 | 它是流水线结果，不是 send 呈现意图；必须按 M03 等待/迁移到 result_route，不能猜成信箱消息。 |
| `stage_pass` | 明确拒绝 | 它是流水线分支自报，且不再具有强制路由权；应归 result 平面。 |
| `bounce` | 明确拒绝 | 它表达流水线换轨，不等价于普通上桌或投信箱；应由 result_route 后片承接。 |
| `blocking` | 映射为上桌 | 兼容期保守保持可见，避免旧阻塞告警被静默；该映射不把自报 class 认作 typed `blocked` 状态事实。 |
| `final_review` | 明确拒绝 | 它是流水线站点/分支语义，不再是 send policy；应归 result 平面。 |
| `timeout` | 映射为上桌 | 兼容期保守保持计划外超时可见；该映射不把旧 class 本身认作 typed timeout 事实。 |

理由：兼容期保护已发布调用者，但 policy 权威必须在本片当场切到“一比特 + 状态事实”，不能保留双写者。若继续让 class 决策，只是换名未收缩；若立即全硬断，则把可机械迁移的普通 send 也升级为用户破坏。

## 五、msg-routing 15 形态连锁影响

处理原则：正确修复导致旧契约失效，必须在同片由验证种系重锚并签字；生产侧不改断言。以下只列语义影响，不修改冻结契约。

| 旧形态 | 本片影响 | 处理 |
|---|---|---|
| form1 默认 leader/message | `class=message` 默认字段失效；默认上桌仍保留 | 重锚为“mailbox omitted → 上桌” |
| form2 未知 sink/class fail loud | class/sink 输入退出 | 重锚为 mailbox 布尔类型/未知字段 fail loud |
| form3 presentation 字段保真 | class/sink/case_id 不再是 send 契约 | send 部分失效；改锁一比特与 canonical body |
| form4 requested/effective/reason durable | class policy reason 失效 | 保留 requested mailbox/effective disposition/事实穿透审计 |
| form5 send casefile durable、无注入 | 行为保留，输入名变化 | 重锚为投信箱，继续锁 durable 正控与零 live inject |
| form6 stage_result result row、无通知 | 属 result 平面 | 本片不改，待 result_route 后片重锚 |
| form7 silent durable/pullable、不注入 | 多 sink 体系退出 | 并入投信箱单一形态，删除独立 silent 形态 |
| form8 五 critical class 强制 leader | 与事实 policy 冲突 | 失效；改锁 failed/fallback/blocked/timeout 事实穿透 |
| form9 typed class anti-regex | class 权威退出 | 失效；改锁正文/status 自报词不改变路由 |
| form10 retry/rebind 保 effective 决策 | 行为仍需 | 输入改为 mailbox；继续锁重启后不误注入 |
| form11 既有 send/report/broadcast/dedupe | 护栏仍有效 | 保留，但 send 调用改新参数 |
| form12 单 C6 primitive/单注入漏斗 | 漏斗纪律仍有效 | 保留，决策 primitive 输入改“一比特 + 状态事实” |
| form13 stage result 可轮询不上屏 | 属 result 平面 | 本片不改，待 result_route 后片重锚 |
| form14 五 critical 各 exactly once 上屏 | 自报 stage 强制面失效 | 替换为四类状态事实 exactly once 穿透 |
| form15 malformed casefile 产 visible bounce | casefile/bounce 旧语义退出 | 重锚为 malformed mailbox fail closed；流水线 bounce 待后片 |

本片直接失效/替换：form1–5、7–10、14–15。  
本片保留但改调用/决策输入：form11–12。  
明确留给 result_route 后片：form6、13，以及 form15 的流水线 bounce 去向。

## 六、与 result_route 片的顺序依赖

| 锚号 | 顺序约束 |
|---|---|
| T01 | 先落本片的一比特 send substrate、旧 class 非权威化和状态事实穿透。 |
| T02 | result_route 后片再承接 result 的 leader/pipeline 路由、status 不透明运输与 stage_* 流水线语义。 |
| T03 | 在 T02 就绪前，send 侧 stage_* 只能进入弃用/明确阻塞态，不能提前删除唯一可恢复入口，也不能继续驱动旧 policy。 |
| T04 | T02 验收通过后，验证种系同片重锚 form6/form13/form15-bounce，并签字 send 侧 stage_* 日落完成。 |

```text
本片：send 一比特 + 状态事实 policy
  │
  ├─先切断 message-class 的路由权威
  │
  └─提供 stage_* 弃用/阻塞边界
             │
             ▼
后片：task.result_route + 不透明 status + result store 消费
             │
             ▼
验证种系重锚 result 形态 → send 侧 stage_* 完成日落
```

## 七、§2d 框架自产系统件评估

**建议：独立后片，不纳入本片，也不拒绝。**

理由：

1. 本片移除的是“让 agent 做全局分类”的负担；框架自产系统件由确定性 producer 产生，是否改变接收方调度可以来自其事件类型/工作流事实，不需要恢复发送方 class。
2. §2d 的收益已有量化锚（框架/系统件约占 leader 收件 11%），例行件默认入箱具有明确价值，故不应拒绝。
3. “例行 advisory”与“触发调度”需要逐 producer 建立 typed 事实目录和正反例；若塞入本片，会把单一 send 收缩扩大成系统事件分类改造，并有重造全局 class 的风险。
4. 应在本片一比特 substrate 稳定后另开后片：框架 producer 只能基于自身可证明的事件事实选择 mailbox；无法证明是否改变调度时默认上桌，禁止读正文猜测。
5. 发版通告若其工作流事实是“立即升级/行动”，必须穿透；纯例行通报默认入箱。该划线属于后片的 producer 契约，不成为 agent 新参数。

后片准入判据：列全框架自产消息 producer、为每类给“是否改变接收方调度”的 typed 事实来源、默认策略、反面正控和未知类 fail-safe；缺一不得进入 policy。

## 八、非目标

1. 不设计 `result_route` 字段、status 词表或编排分支。
2. 不把死结诊断边、轮次、阈值、下一站搬进框架。
3. 不用新的枚举替换旧八值 class。
4. 不从正文、summary 或发送方自报 stage 词猜路由。
5. 不在本页修改 msg-routing 冻结契约；只声明验证种系同片重锚范围。
