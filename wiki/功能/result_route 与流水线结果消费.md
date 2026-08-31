---
name: result_route 与流水线结果消费
type: requirement
status_at: 0.5.60
target_version: 0.5.61
domains: F5 F6 F10
---

# `result_route` 与流水线结果消费

本页承接 [[send 单信箱与状态事实穿透]] T02–T04：send 平面先收缩成一比特，result 平面再用 task 上的 `result_route` 承载结果去向和 stage_* 流水线语义。

typed 失败事实共同 SSOT：[[typed 状态事实目录与强制穿透]]。

唯一需求源：`.team/artifacts/0.5.60-message-plane-requirements-20260725.md` §1【二】【三】、§2③–④、§2b、§2c。

## 一、用户获得什么

### 行为承诺

| 锚号 | 承诺 |
|---|---|
| B01 | task 可声明 `result_route: leader | pipeline`；字段在派单时外部设置，唯一写者是派单方。 |
| B02 | 未声明 route 时默认为 `leader`，保持现有任务与 worker 零迁移：普通最终结果仍进入 leader。 |
| B03 | worker 不读取、不填写、不覆盖 route；无论 route 如何，worker 仍只 `report_result` exactly once，结果先 durable。 |
| B04 | route=`pipeline` 时，结果进入 result store，由编排脚本消费并触发下一站；leader 侧同时形成可回溯 casefile 归档，不即时上桌。 |
| B05 | worker 结构化终报 envelope 层的 typed failed/fallback/timeout，以及 result 接收/路由过程产生的计划外失败事实，都无条件穿透 leader，不受 route=`pipeline` 约束；穿透时仍保留 durable result/失败引用。业务 status 字符串不属于这一层。 |
| B06 | result 的业务 `status` 是不透明字符串：框架只原样运输和持久化，不维护词表、不解释、不归一化、不据此选择 route 或下一站。 |
| B07 | status 词表、每值说明、轮次、阈值、分支表、预期异常和下一站只属于编排脚本；脚本把本任务词表组装进派单提示词。 |
| B08 | stage_result、stage_pass、bounce、final_review 等流水线语义从 send/presentation 平面搬到 result 平面；这是归位，不是删除阶段能力。 |
| B09 | 两种 route 共用同一 result_id、任务归属、durable、exactly-once、恢复和审计语义；切 route 只改变消费目的地。 |

### 为什么 route 在 task、status 对框架不透明

1. **参数落在信息所在的地方**：结果去向在编排设计/派单时已知，必须写在 task 上；执行时才让 worker 决定会制造第二写者。
2. **worker 报事实、脚本持状态**：worker 只报 status、失败签名和报告引用，传引用不传值；确定性的轮次和分支由脚本维护。
3. **任务 outcome 不是全局 class**：outcome/status 是每任务字段，由派单方定义词表、脚本消费；message-class 是发送方揣摩的全局分类作业。允许前者不构成恢复后者的理由。
4. **预声明消化可预见异常**：如“三轮仍红→审计站”已写入脚本，就是计划内 pipeline 分支；穿透只留真正计划外失败。
5. **死结诊断边不进框架**：根因分类、诊断站和新增站型属于编排素材；框架只提供 route、durable result 与失败事实。

`failed/fallback/timeout` 穿透与 status 不透明并不冲突：穿透层包含 worker 结构化终报 envelope 的 typed 失败事实，以及 result 接收/路由过程可证明的框架失败事实；它们都不从任意 status 字符串猜测。业务 status 是任务自定义词表，哪怕恰写 `"failed"` 也不能让框架擅自把 pipeline 结果升级上桌。

## 二、怎样黑盒判真

### route 求值与唯一写者

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C01 | B01/B02 | task 省略 route，worker 正常完成 | effective route=`leader`；结果 durable 且恰一次进入 leader |
| C02 | B01/B04 | 派单方设 route=`pipeline`，worker 正常完成 | result store 有同一 result_id；leader 主屏无 live result；casefile 可回溯 |
| C03 | B01/B03 | worker 尝试在结果中填/覆盖 route | 明确拒绝该字段或忽略为无权输入，task route 不变，结果/任务状态无部分污染 |
| C04 | B01 | task route 为未知值 | 派单/任务激活前 fail closed，不产生可执行半任务 |
| C05 | B02/B09 | 同一任务事实分别省略 route、显式 leader | 两者观察等价；result_id 只各有一份，无迁移差异 |

### pipeline 观察面与幂等消费

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C06 | B04/B09 | pipeline task 产一个唯一结果 | 编排脚本可按 task/result 引用拉取完整事实；无需读 leader 对话或解析摘要 |
| C07 | B04/B09 | consumer 拉取后在提交 checkpoint 前中断，再恢复 | 同一 result 可重读但只触发一次下一站；不得复制 result 或推进两轮 |
| C08 | B04/B09 | 重复 poll/collect/restart 同一 result | result_id、canonical 内容与 casefile 归档不变；下一站消费具稳定幂等键 |
| C09 | B04 | leader 主屏保持 idle，不做 collect | pipeline result 仍 durable、可由脚本消费；leader casefile 可查询但不冒充 notified |
| C10 | B03/B09 | worker 重复 `report_result` 或零次报告 | 重复与缺失均失败；route 不改变 exactly-once 判据 |

观察面需求：

1. result store 必须按稳定 task/result 引用提供可恢复拉取。
2. 每个 result 有稳定消费身份和可持久 checkpoint；重复拉取允许，重复触发下一站不允许。
3. casefile 归档与 pipeline 消费引用同一 canonical result，不复制一份可漂移正文。
4. 拉取、checkpoint 或下一站触发失败必须留下可诊断事实，不得把 result 标成已消费后再丢失触发。

### status 与失败穿透

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C11 | B05 | pipeline task 分别提交 envelope 层 typed failed/fallback/timeout，并分别制造接收/路由过程失败 | 每种 typed/框架失败事实都无条件恰一次上桌，同时保留 durable result/失败引用；与 C12 的自由 status 字符串形成差分 |
| C12 | B06/B07 | 依次提交多个自定义 status，包括未知值和字符串 `"failed"` | 每值 byte/语义原样进入 store 与脚本；框架不拒绝、不翻译、不分支；字符串本身不触发穿透 |
| C13 | B06/B07 | summary 含 stage/timeout/final 等词 | route 与 status 均不改变，证明框架不读正文猜流程 |
| C14 | B07 | 编排脚本对同一 status 在不同 task 词表中定义不同分支 | 两任务各按自己的脚本分支；框架观察一致且无全局词表 |

### stage_* 承接

| 锚号 | 覆盖 B | 输入与动作 | 接收侧/落盘判据 |
|---|---|---|---|
| C15 | B08 | pipeline task 报 stage_result/stage_pass/final_review 对应的自定义 outcome | 结果只作为不透明事实进入 store，由脚本按本任务词表分支；send policy 不参与 |
| C16 | B08 | 脚本产生 bounce/换轨分支 | 下一站由脚本 checkpoint 后恰一次触发；不要求 worker 发送 `message-class=bounce` |
| C17 | B08 | 使用已日落的 send stage_* 入口 | 按 [[send 单信箱与状态事实穿透]] M03/S-011 明确拒绝或迁移适配；不得形成第二套 route |

## 三、现在处于什么状态

**0.5.60 已发布时点：未实现本契约；0.5.61 计划态。**

已发布可复用能力：

- 结构化 `report_result`、durable result store、result_id/任务归属和 casefile/stored-only 行为已经存在。

仍未满足：

1. route 尚未成为 task 上由派单方唯一写入的 `leader | pipeline` 字段。
2. 默认 leader 与 pipeline 消费尚未形成同一求值、持久拉取、checkpoint 和下一站幂等闭环。
3. 既有 stage_result + case_id/presentation 仍在 result 接收面强制分流，status 仍有被框架解释/归一化的历史债。

本页是需求状态，不预支实现；现状实现距离由 runtime-owner 在目标 `origin/main` 上另行核验。

## 四、兼容与迁移

| 锚号 | 建议 |
|---|---|
| M01 | 默认 route=`leader` 从第一版生效，使未更新 task、角色和 worker 保持零迁移。 |
| M02 | `result_route` 只允许派单入口写；旧 worker payload 无需新增字段，官方 worker contract 不出现 route 选择。 |
| M03 | 旧 stage_result + case_id/presentation 进入 S-011 所裁兼容窗口：兼容层只读迁移到 task route，不能与新 route 并列为 policy 写者。 |
| M04 | 若旧 stage_* 无法无歧义关联到 task route，明确拒绝并给迁移指针；不得猜成 leader 或 pipeline。 |
| M05 | pipeline consumer 首次上线前必须提供 checkpoint/dedupe 与 casefile 同源证明；只有“能拉取”不足以发布。 |

本页没有新增 S-012：stage_* 兼容窗口和旧冻结契约终止点已由 S-011 完整圈定；重复挂号会把同一用户选择拆成两债。若后续出现“pipeline 消费后 result 保留期/人工重放权”等新的用户选择，再新立 S-012。

## 五、承接 msg-routing 15 形态

本页只承接姊妹页留后的三项；其余 form 仍按 [[send 单信箱与状态事实穿透]] §五处理。

| 旧形态 | 新需求 | 重锚要求 |
|---|---|---|
| form6 stage_result result row、无 leader notification | route=`pipeline` 的 result durable + leader casefile | 改为 task route 决定目的地；默认 leader 正控、pipeline durable/零 live inject 反面齿同测 |
| form13 stage result 可轮询不上屏 | pipeline result store 消费观察面 | 加 stable result id、可恢复拉取、checkpoint/dedupe、casefile 同源与非 vacuous durable 正控 |
| form15-bounce malformed casefile 产 visible bounce | route/schema 错误 fail closed；流水线 bounce 由脚本分支 | malformed task route 在激活前拒绝且零污染；合法脚本 bounce 恰一次触发下一站，不再依赖 send class |

连锁签字顺序：

1. 本页 C01–C17 冻结为新 result_route 契约。
2. form6/form13/form15-bounce 由验证种系同片重锚，生产侧不改断言。
3. 新契约在已发布基线红因正确、候选全绿。
4. S-011 的兼容窗口满足后，签字 send 侧 stage_* 日落完成。

## 六、与四站生产链的消费关系

§2c 四站是 route=`pipeline` 的首个正式消费者：

```text
需求 wiki
  → 拼装基底站
  → 测试分析站（用例设计文档落盘）
  → 测试点导出站
  → 脚本编译执行站
```

每一站：

1. task 由派单脚本预声明 `result_route=pipeline`。
2. worker 只报告 status、失败签名和产物引用。
3. result durable 后，脚本按 result_id 拉取、写 checkpoint，再触发下一站。
4. 下一站只接收产物引用，不复制大值；leader casefile 保留全链可追溯索引。
5. 计划内 outcome 按本任务词表走边；真正 failed/fallback 穿透 leader。
6. 只有站型、拓扑或分支表需要改变时才升 leader；框架不参与死结根因判断。

四站拓扑与词表是编排素材，不是本页新增框架能力；本页只保证它可用 route、store、checkpoint 和失败穿透可靠消费结果。

## 七、顺序约束

| 锚号 | 约束 |
|---|---|
| T01 | [[send 单信箱与状态事实穿透]] 先切断 message-class 的 send 路由权威，并提供 stage_* 弃用/阻塞边界。 |
| T02 | 本页再引入 task `result_route`、默认 leader、pipeline store 消费和 status 不透明运输。 |
| T03 | 本页 C01–C17 与 form6/form13/form15-bounce 重锚全绿后，stage_* 才算完成从 send 到 result 的语义搬家。 |
| T04 | S-011 兼容窗口结束后删除旧入口；删除前也不得让旧 presentation 与新 task route 同时成为写者。 |

## 八、非目标

1. 不定义任何全局 status 枚举。
2. 不让 worker 选择或推断 result route。
3. 不把四站拓扑、轮次、阈值、死结诊断或下一站选择写进 runtime。
4. 不以解析 summary/status 文案实现 failed/fallback 穿透。
5. 不修改 send 一比特契约。

## 九、实现位置（2026-07-29 亲核）

> **本段存在的理由**：2026-07-29，leader 花一整天向外部顾问求证本页 B01/B04/B06/B07 已答明的问题，并一度断言"该能力完全不存在"——实际实现早在未合并分支上。**根因是本页只写"要什么"、不写"做到哪了"**，读者读完只能推断"尚未实现"。**`type: requirement` 的页缺这一段，等于邀请重复实现。**

| 项 | 值 |
|---|---|
| **状态** | **已实现，未合并** |
| **分支** | `feature/orch-admission-g01-g02-g09-d15`（产品仓 `/Users/alauda/Documents/code/team-agent-public`） |
| **与 main 关系** | 领先 `main` **287** commit，落后 **0**（`git rev-list --left-right --count main...<branch>` = `0 287`） |
| **main 现状** | **无**。`git grep -c result_route main -- crates/` 空。已发布的 **0.5.60 不含本能力** |
| **关键 commit** | `206e65f`（移除 `presentation` 第二写者，改为 task route 唯一权威） |

**实现文件（6）**

| 文件 | 承担 |
|---|---|
| `crates/team-agent/src/model/spec.rs:637` | `result_route` 进 task spec（B01：派单时外部设置） |
| `crates/team-agent/src/messaging/task_admission.rs:213` | 非 `leader`/`pipeline` → `UnknownResultRoute`，`assign_task` fail closed（C04） |
| `crates/team-agent/src/messaging/results.rs:1078` | `canonical_result_presentation`：Leader→Leader / Pipeline→Casefile，`case_id` = `task_id`（B04） |
| `crates/team-agent/src/messaging/results.rs:1099` | `validate_presentation_advice`：worker 提交的 presentation 降为 advice，与 task route 冲突即拒（B03/C03） |
| `crates/team-agent/src/messaging/presentation.rs` | stage_* 类要求 result route（B08/T04） |
| `crates/team-agent/src/mcp_server/wire.rs` | MCP 入口 |

**RED（2）**：`tests/result_route_task_contract_red.rs`、`tests/result_route_mcp_ingress_contract_red.rs`

**未合并原因**：该分支 287 commit 混有其他工作，不能整分支合；需把 `result_route` 竖切为独立分支单独验证、单独合入。**截至 2026-07-29 未做。**

**B06 独立核实结论（与实现分支无关，`main` 上即成立）**：框架对业务 `status` 确实只运输不解释——`messaging/helpers.rs:83 validate_result_envelope` 只校验 status 存在且为字符串，无值白名单；MCP `wire.rs` 走 `report_result_with_presentation`，status 取 `args.get("status").and_then(Value::as_str)` 原样透传。
**但仓内存在两个已写好、未接线的 status 解释器**：`mcp_server/normalize.rs:33 normalize_result_status`（四值归一化，未知字面 → `Partial`；当前仅被 `tests/golden.rs` 调用）与 enum 版 `tools.rs:365 report_result(status: ResultStatus)`（Rust 库 API，不在 MCP 路径）。**任何人把它们接进 MCP 入口，B06 当天失效、编排自定义词表全部塌成 `Partial`。** 建议补一枚齿锁死"MCP 提交的 status 必须原样出现在 store"。

---

## 本页维护规矩（适用于 `wiki/功能/` 全部 `type: requirement` 页）

**每页必须有「实现位置」段，不得省略。** 未实现也要显式写 `状态：未实现`，而不是留空——**留空与"没做"不可区分，正是 2026-07-29 那次重复劳动的机制成因。**

最少四格：**状态**（未实现／已实现未合并／已合并+版本）、**分支或 commit**、**与 main 的关系**、**未合并原因**。
