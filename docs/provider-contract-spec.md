# 新 Provider 适配标准契约规范与接入指南

**New Provider Adaptation Contract & Specification — Astra 独立提案 v1**

- 状态：**设计提案，尚未实施，也不是六家 Provider 的实机认证**。
- 事实基线：[`8ff50cfa21c868ff08bf27a36ed8447154b69cc5`](https://github.com/Florious95/team-agent/tree/8ff50cfa21c868ff08bf27a36ed8447154b69cc5)，tree `dfef23ca1611faffbdb10bcf253ecfe627139c82`，产品版本 `0.5.112`。
- **现状**指该基线的函数体及生产调用；**规范**指新接入必须满足的目标；**拟议 API**均尚不存在。不得把后两者当成当前 API 使用。
- 范围：模型发现到停止清场的完整接入；六家现状兼容边界；第七家虚构 Provider 的教程。不改产品、提示词、公开命令集合或持久格式，不授权迁移、发布、全局配置修改。

## 1. 目标、边界与共同语言

目标不是“每家都实现同一批方法”，而是：开发者能声明一个**可达操作**，沿唯一交付链证明它完成到哪一步；不能证明的部分有明确停止点。六家普通 worker 都运行 native CLI，差别在配置、会话、输入与证据，不是“API Provider / CLI Provider”二分。

### 1.1 三个责任边界

| 所有者 | 负责 | 不负责 |
|---|---|---|
| Provider 层 | native argv、目录解析、输入界面识别、会话格式、MCP 载体及能力限制 | 创建第二套队列、选择 Team owner、直接杀进程、决定宿主拓扑 |
| Lifecycle / messaging | 规格准入、身份、资源物化、队列、操作编排、收据、补偿 | 猜 native flag、把 regex 命中当业务成功 |
| Transport / host | endpoint/pane 寻址、shell、buffer、逻辑键、capture、有限 OS effects | 推断模型能力、自动授予权限、从 Provider 名称决定消息语义 |

现有 `ProviderAdapter` 主要由一个 `BasicProviderAdapter` 实现；`provider/adapters/*.rs` 是 provider-local 自由函数，不要求新建第七个 Adapter 类。复用该 facade、`CommandPlan`、`Transport`、统一消息存储。**不引入插件总线、通用脚本 DSL 或新的生命周期执行引擎。**

### 1.2 能力与证据必须分开

每项能力用 `(provider, operation, auth_mode, native_version, channel, backend)` 定位，记录四列，不压成一个 `supports=true`：

1. **Declared**：实现声明了什么模式及限制。
2. **Reachable**：哪个公开入口实际到达该实现；只有 helper/单测不算。
3. **Observed**：本次实例观察到什么，证据源、时间、身份是什么。
4. **Verified**：哪个有限验收断言成立；注明模型、版本、平台、场景。

能力准入分 `Supported(mode, prerequisites)`、`Unsupported(reason)`、`Unverified(reason)`。后两者对所请求操作均不执行危险 effects，但原因不同。暂时未登录/找不到 executable 是 `Unavailable`，不是永久 Unsupported。

观测结果至少区分 `Observed(value)`、`Absent`、`Unknown/Unverifiable`、`Unsupported`、`TimedOut`、`Error`；只有成功查询且覆盖所需范围才能给 Absent。没有读到、未运行、超时、返回空表、pane 活着，不能互相替换。

**规范 C01 — 身份先于结论。** 所有副作用/收据绑定 canonical workspace、owner team、agent id、spawn generation、backend endpoint、pane/session；native session id、backing path、message id、result id 分字段保存。`%3` 不跨 endpoint 唯一；预分配 UUID 不等于捕获成功；wrapper PID 不等于 Provider PID。

### 1.3 六家现状不是统一能力表

| Provider | 已接入的主要载体 | 必须保留的限制 |
|---|---|---|
| Codex | `-c` 配置、JSONL turn/fault、native catalog；leader 另有 app-server | `caps.fork=true` 不等公开 `fork-agent` 可用；普通 worker 仍走 CLI |
| Claude / ClaudeCode | prompt/permission flags、inline/file/managed MCP、projects root、JSONL | 两个 wire 值同一 executable；native/compatible 的会话与 profile 门不同 |
| Pi | owned extension、runtime MCP、live exact catalog、exact-path resume、完整快照新席 fork | 旧 adapter launch/resume 不适用；没有专用 semantic turn reader；单 Enter helper 未生产接线 |
| CursorAgent | isolated project、rules、MCP 文件与 enable、archive/chatId | 所有 role effort 拒绝；输入重按有 busy guard；不得改 HOME 来隔离 |
| Grok | rules/session argv、目录 MCP、login/trust、原窗 `/fork`、native queue | 目录配置有独占限制；catalog 可见不是登录证明；队列收下不是新 turn |
| GeminiCli | 最小 `gemini [--model RAW]`、共享宿主、遗留 global installer | 无公开 native leader verb / catalog / resume / fork；普通 spawn MCP 连通未证 |

“第七家”是本六家比较矩阵之外的新例子，不是声称 enum 只有六个值；Copilot、Fake 及历史 alias 仍存在。

## 2. 标准生命周期流水线

以下是**契约检查点**，不是要求把现有代码搬进一个大状态机。现有九个 `LifecyclePhase` 是时间事件标签，不能证明对应业务门已通过。

| 阶段 | 输入 → 必须产出的事实 | 副作用、失败与恢复边界 |
|---|---|---|
| P0 解析操作与归属 | role/spec/CLI、selected team → operation、scope、入口模式 | 拒未知 provider/冲突/越权；不从相似名称猜 provider；冻结本次实例身份 |
| P1 解析配置 | model 来源、auth/profile、effort、bypass → 有来源的有效配置 | role 显式优先；缺省不制造模型/effort；profile 错误不暗降级 |
| P2 发现与准入 | native catalog 能力、可选 model → catalog / selection decision | 发现是有界 I/O；不需要 discovery 的模式不强制调用；Unsupported 不伪造空目录 |
| P3 描述命令 | 已准入配置、会话选择 → `CommandPlan` / native argv | raw argv 与 shell quoting 分开；资源 ID 分配显式记录；不能在 builder 偷装 MCP |
| P4 物化资源 | plan、MCP/role、seat paths → owned files、env overlay/unset、补偿清单 | 每个写入有 scope/owner；只恢复仍属于本操作的内容；不回滚整个 workspace |
| P5 选择宿主与 spawn | operation、endpoint、plan、env → returned pane + owner/generation | direct / worker / managed leader 由操作选择；argv-route 仅 native invocation 一次 |
| P6 核验实例 | returned pane、OS/host 观察 → L1/L2 独立收据 | 错绑只处理本操作创建的 pane；不从首个窗口冒认成功；失败不宣称 Provider 已启动 |
| P7 协议可用 | owned MCP binding、实际工具目录 → L3 Available 或明确阻塞 | 注册不等工具已返回；按载体 gate 初始输入，不无限等待业务回包才允许第一条任务 |
| P8 投递与执行 | durable message、收件 channel、输入契约 → 物理 effect / native receipt | 一次 paste 与有界提交；未知提交状态进入观察，不自动重投 |
| P9 回程与结果 | 当前消息/turn 的 native MCP 调用 → durable outbound / result / 通知状态 | accepted、submit、业务结果、leader 显示分别报告；presentation 可以刻意不 live |
| P10 捕获与恢复 | session candidate、cwd、cohort → authoritative resume binding | sid/root/path/身份不足则拒绝 resume；不自动 fresh，不以最近 mtime 抢别席会话 |
| P11 停止与清场 | scoped ownership、资源清单 → effects、preserved、fresh residuals | 先记真实 effects；未知/共享对象保留；clean 必须有复验，不由退出码推断 |

捕获与健康观察可贯穿 P6–P10，不必等任务结束才扫描。每个可能产生 effect 的阶段记录 `operation_id`、before/after 身份、effect、错误、剩余预算；文件内容/令牌/正文不进入普通日志。

**规范 C02 — 生命周期复用。** fresh、add/start、restart、fork target 必须复用同一 provider-local 准入/物化逻辑和 shared spawn，不能各自复制 argv 与 MCP 安装。

| 操作 | 流水线差异 |
|---|---|
| cold fresh / display split | 通常 direct shell；允许 native 未指定模型的缺省 |
| dynamic add/start/restart | worker wrapper；停止旧实例与准备新实例是分立 effects，不宣称事务无副作用 |
| resume | P3 接受已验证 binding；保原会话，失败拒绝，不冒充 fresh 成功 |
| reset | 必须显式 discard-session；停止前后检查残留，bump generation；paused 不强行 spawn |
| native leader | 保留 native 参数入口；Pi 有专门参数准入；不可强套 worker 的 role 解析 |
| fork | 先选明确定义的 fork 模式，再进入 §7；不是统一调用旧 `fork()` |
| stop/remove/shutdown | 走 §8；不把停止当作删除用户会话或全局配置的许可 |

## 3. 行为 → 模块 / Trait / Hook / 函数地图

下表路径相对 `crates/team-agent/src/`。标 **现有** 的符号在固定基线存在；**新增槽位**是本提案的接线目标，不能在当前 HEAD 直接调用。

| 要实现的行为 | 从哪里接线 | 接入者负责什么 |
|---|---|---|
| canonical provider、alias、binary | 现有 [`model/enums.rs`](../crates/team-agent/src/model/enums.rs) `Provider`；[`provider/wire.rs`](../crates/team-agent/src/provider/wire.rs) `provider_wire/parse_provider/parse_canonical_provider/command_name` | 分开 canonical spec 与历史 alias；不顺手扩公开 leader verb |
| role/spec、CLI 准入 | 现有 [`compiler.rs`](../crates/team-agent/src/compiler.rs) `compile_role_agent/compile_team`；[`model/spec.rs`](../crates/team-agent/src/model/spec.rs) `validate_spec`；`cli/adapters.rs` | 正常入口与 direct spec 都校验；更新适用 schema/帮助文本，不用 fallback Codex 接住漏项 |
| 模型目录与搜索 | 现有 [`provider/model_catalog.rs`](../crates/team-agent/src/provider/model_catalog.rs) `catalog_source/discover_model_catalog/model_matches`；`cli/models.rs` | native source/parser 独立；共享 bounded runner/search；支持与不支持都写显式分支 |
| Effort | 现有 `ProviderEffort::parse/resolve_for_provider`；compiler、launch/restart caller | 通过 / known-ignore + warning / 拒绝三态；声明继承规则和 native flag，不改旧错误全文 |
| profile 与认证 | 现有 [`lifecycle/profile_launch.rs`](../crates/team-agent/src/lifecycle/profile_launch.rs) `prepare_provider_profile_launch_with_profile_dir/prepare_provider_profile_launch_from_json/provider_env_exports/provider_env_unsets/provider_command_overrides` | 实现真实 auth mapper；被 spec 接受不是 BYOK 已实现 |
| 身份/prompt/环境 | 现有 `lifecycle/worker_command_context.rs` `from_yaml/from_json/compile_worker_system_prompt`；`lifecycle/launch/worker_env.rs` `inherited_env_with_team_overrides/apply_profile_launch_env` | 消费编译后的 prompt；保 identity/MCP/communication/body/output 顺序与最终隔离 |
| fresh / resume argv | 现有 [`provider/adapter.rs`](../crates/team-agent/src/provider/adapter.rs) `ProviderAdapter::build_command_plan/build_resume_command_plan`；`provider/adapters/*.rs` | 在 `BasicProviderAdapter` dispatch 增分支，provider-local 函数造 raw argv；特殊 exact-path 用专属物化入口 |
| 命令结构 / shared append | 现有 [`provider/types.rs`](../crates/team-agent/src/provider/types.rs) `ProviderCommandContext/CommandPlan`；`provider/command_helpers.rs` | 复用原始 pair/optional append，不在 helper 加 trim、escape、排序、去重 |
| MCP 描述/文件 | 现有 `ProviderAdapter::mcp_config/install_mcp`；`lifecycle/launch/mcp_config.rs::resolve_mcp_config` | 给出载体和真实读取路径；installer 定义不算 wired；有副作用安装在 lifecycle |
| runtime MCP bridge | 现有 `lifecycle/launch/pi_mcp.rs::materialize_pi_plan/materialize_pi_resume_plan`；`pi_mcp_extension.js`（仅 Pi） | 新 Provider 若需 bridge，做自己有官方接口的窄物化/观察模块，不冒用 Pi extension |
| 实际 launch 入口 | 现有 [`lifecycle/launch/spawn.rs`](../crates/team-agent/src/lifecycle/launch/spawn.rs) `spawn_agents`；`lifecycle/restart/common.rs::spawn_agent_window`；`leader/start.rs` | 三条入口显式使用同一配置决策；辅助 catalog/version/MCP enable 不经过 host argv-route |
| 宿主 shell | 现有 [`tmux_backend.rs`](../crates/team-agent/src/tmux_backend.rs) `shell_command/worker_shell_wrapper_command/leader_shell_wrapper_command/spawn_with_command` | 通常不改；接 operation policy，不塞 Provider if/else |
| Agent → Team | 现有 [`mcp_server/wire.rs`](../crates/team-agent/src/mcp_server/wire.rs) `dispatch_tool`；`mcp_server/tools.rs::send_message_with_presentation/report_result_with_presentation` | native 工具名映射到现有三 logical tools；身份由框架注入 |
| Team → Agent | 现有 [`messaging/delivery.rs`](../crates/team-agent/src/messaging/delivery.rs) `deliver_pending_messages/deliver_prepared_message/render_message` | 复用 persist-first funnel，按收件 channel 选协议；不得另建 provider send 队列 |
| 输入策略/识别 | **新增槽位** `ProviderAdapter::input_contract` / provider-local `classify_input` 回调，见 §5 | 前者纯数据选择，后者纯 capture 解释；在 `adapters/<new>.rs` 实现，不自己敲键 |
| 契约式物理执行 | 现有 [`transport.rs`](../crates/team-agent/src/transport.rs) `inject/inject_with_submit_observer/send_keys/capture`；**新增槽位** `Transport::inject_with_contract` | 显式传 policy/identity；Tmux 实现一次 paste + 有界键，其他 backend 不支持时拒绝，不静默降级旧 inject |
| native 无键投递 | 现有 `codex_app_server.rs::submit_to_bound_thread/turn_start`；`delivery.rs::deliver_leader_via_app_server` | 绑定 socket/thread 元组；不是通用 CLI worker 通道 |
| submit / receipt / result | 现有 `delivery.rs::inject_submit_verified/observe_leader_receipt/CurrentTurnSubmitObserver`；`messaging/results.rs::report_result_for_owner_team_inner` | 保留当前兼容映射；新细分 evidence 不直接改旧 durable status |
| L1/L2 probe | 现有 `coordinator/steps/abnormal.rs::agent_process_liveness`；`os_probe.rs`；`Transport::has_pane/liveness/provider_exit_status`；`restart/rebuild.rs::restart_readiness` | 使用共享 host/OS 观察，不能每家复制 kill/probe；wrapper、native process 与 pane 分开 |
| L3 probe / semantic activity | 现有 Pi runtime events；`coordinator/tick.rs::jsonl_activity_for_agent`；`provider/classify.rs`；`messaging/activity.rs` | 新载体的协议 observer 在其 lifecycle MCP 模块接入 readiness；无 reader 则 Unknown，不是空闲 |
| 会话归属/恢复 | 现有 `ProviderAdapter::capture_session_candidates/recover_session_id`；`provider/session_scan/`、`provider/session/` | 返回候选与置信依据，由框架按席分配；exact backing 不降为 sid-only |
| 公开 fork | 现有 [`lifecycle/launch/fork_agent.rs`](../crates/team-agent/src/lifecycle/launch/fork_agent.rs) `fork_agent_with_transport/in_window_fork`；[`lifecycle/restart/avatar.rs`](../crates/team-agent/src/lifecycle/restart/avatar.rs) `fork_pi_new_seat_locked` | **拟议**在此引入模式化准入/结果；旧 `ProviderAdapter::fork_plan/materialize_fork_backing` 仅 native plan 层 |
| stop/reset/remove/shutdown | 现有 `lifecycle/restart/agent.rs::stop_agent_at_paths/reset_agent_at_paths`；`restart/remove.rs::remove_agent_inner`；`cli/mod.rs::lifecycle_port::shutdown_with_transport_and_state` | 复用 scoped teardown，提供 owned 资源信息，不新增按进程名清理器 |

这张表是导航，不是把所有文件都列为每次接入的必改清单。例如只有 CLI launch 不代表必须增加 native leader 命令；不支持 fork 的 Provider 应接安全拒绝，不写假 fork 实现。

## 4. 模型、Effort、命令与宿主合同

### 4.1 模型与 Effort

**规范 C03 — 发现、选择、准入三分。** 使用一次有界 native catalog observation；当前公共 runner 上限 10 秒、1 MiB。解析完整响应再过滤；保 canonical ID 字节、native 顺序、真实 aliases/default；重复/畸形/空源拒绝，不 partial-success。搜索复用 token-AND / field-OR，不替用户选模型，no-match 与 Unsupported 不同。

模型策略可以是 `NativeDefaultOrOpaqueId` 或 `ExactCatalogId`，须显式说明。未提供模型默认不加 flag；发现目录不证明订阅推理权限。不得统一 sort/trim/大小写化、合并近似 ID 或把模型后缀推导为 effort。

当前兼容矩阵（S=原档透传；I=已知忽略且 warning；E=拒绝）：

| Provider | low | medium | high | xhigh | max | ultra |
|---|---|---|---|---|---|---|
| Codex | S | S | S | S | S | S |
| Claude | S | S | S | S | S | E |
| Pi | S | S | S | S | S | E |
| Cursor | E | E | E | E | E | E |
| Grok | S | S | S | S | E | E |
| Gemini | I | I | I | I | E | E |

role effort 优先 TEAM；Pi 不继承 TEAM；两者没有就 native default。`ultra` 含 Codex client delegation，不准降成 max。支持 flag 不证明每模型支持该档。新 Provider 对六档逐项作准入决策；新接入不复制 Gemini 的历史忽略习惯作为缺省。

Pi 特例保留：compile shape 校验不是 launch exact membership；首次 slash 两边非空不意味着只能一个 slash；内部空格可属于 exact ID；final raw 与 trim 不等要拒绝。非 Pi `validate_model=Ok(true)` 不是强验证；`verify_started_pi_model` 有定义/测试不等生产已用。

### 4.2 纯描述与物化

**规范 C04 — 命令不是 shell。** provider-local builder 只拼 argv；配置 JSON、Codex instructions escape、POSIX shellquote、YAML quoting 各用对应函数，不能共享一个“万能 escape”。`Some("")` 是否传给 native 由已声明入口决定，不能在 shared append 偷修。

目标设计中，将检查 executable/catalog、写配置/role/extension、分配/记录资源身份放在可失败的 materialization 阶段；纯 argv 组装使用已解析输入。当前 `build_command_plan` 有 UUID 分配，不能因此宣称整个 trait 已纯化；不为纯度重写现有调用。

- MCP server 必须指向当前 candidate 的绝对 executable，使用既有 `mcp-server --workspace ...` 与 captured Team identity。namespace 由 native client/runtime binding 决定，不猜固定前缀。
- 声明载体为 `ArgvConfig / ScopedFile / RuntimeBridge / Unsupported`，另记 scope（invocation / seat / cwd / global）。`native_mcp_config=false` 不等于没有 MCP。
- scoped 文件有 owner、权限、原内容/写入摘要与恢复条件。需要 global settings 的新接入必须单独获授权并有 compare-owned-content 恢复；不得照搬 Gemini installer 注释承诺不存在的 backup/restore。
- subscription、official_api、compatible_api 分别准入。没有 credential mapper 的模式拒绝/标未支持，不能靠 metadata 接受来宣称可用。日志只记录变量名与去敏原因，不记录凭据。

### 4.3 Env 与宿主

**规范 C05 — 最终身份与 unset 是安全边界。** 复用 inherited env 隔离、Team overlay、profile overlay/unset、Provider overlay、最终隔离；父 leader/caller 身份不能带入 worker。普通 key 同时 overlay/unset 时，最终 unset 胜出。cold/dynamic 当前顺序不完全一致，迁移先做各入口 characterization，不凭概念图重排。

caller-provider/pane/socket 只在 native invocation 中绑定实际目标 shell，不泄漏进 inert tail。环境文件的物化不意味着 spawn 一定 source 它；不改 HOME 掩盖 provider scope 冲突。

| Host policy | 当前 native 执行方式 | Provider 退出后 |
|---|---|---|
| Direct | cwd/unset/env/caller context 后 `exec argv` | pane 可 dead |
| Worker | 同类 prefix，native 作 child | 记录 worker marker/rc，进入不读 stdin 的 inert shell |
| ManagedLeader | 独立 leader wrapper | numeric pane-option authentic receipt + marker + inert tail |

P5 必须核 returned pane 对应请求 session/window 与 endpoint；owner generation 和 native session 分开。当前 final shell command 上限 16,000 bytes，与文本 buffer 的 16 KiB 分流阈值无关。超限在 spawn 前拒绝。不能为了“统一 shell”删除 leader authentic receipt 或把 inert shell 当活 Provider。

## 5. 消息发送与微观物理通信合同

### 5.1 两个方向、三个结果

**规范 C06 — 发件与收件解耦。** Agent → Team 使用 native MCP client/owned bridge；Team → Agent 才使用收件端输入协议。A 用 Claude 发信给 Cursor，输入键由 Cursor 收件 channel 决定，不由 Claude 决定。

逻辑 MCP 工具仍为 `send_message`、`report_result`、`get_team_status`，不另发明 Provider 私有业务 API：

- `send_message(to, content, ...)` 经身份/scope/presentation 准入，先持久化真实 message id，再返回 accepted/queued 或 stored-only。框架注入 sender/task/schema identity，worker 不可自报替身。
- `report_result(summary, ...)` 校验并持久化结果/归属；`ok=true, result_id` 可与 `leader_notified=false, notification_status=queued` 同时成立。
- `mailbox/casefile/silent` 可以有意不注入 pane。不能把无 live 通知当工具失败，也不能把结果写入当 leader 已收到。

必须分别报告：**durable accepted → physical/native acceptance → correlated business return**。只有最后一项能证明 Agent 实际使用绑定的 Team 工具回程；终端出现“done”、turn/busy、输入 token 都不能代替它。

### 5.2 Envelope、分隔符与发送函数

共同 `render_message` 的兼容字节形状：

```text
Team Agent message from <sender>[ for <task_id>]:

<content>

[team-agent-token:<message_id>]
```

最后 `]` 后**没有额外 LF**。content 原文保留 LF/CR/引号；正文换行不是提交键。末尾 token 是 Team correlation marker，不是 secret、native session id、模型 stop sequence 或回复结束符。

**规范 C07 — token 身份显式传递。** 拟议发送请求携带可信 `message_id` 和 renderer 产生的 marker/range；不能从任意正文“取第一个 token”作为权威。基线低层正这样取第一个，而高层 receipt 查实际 message id：迁移此差异必须单独有碰撞 fixture，不悄悄修改旧路径。

拟议 API（接口草图，不是可编译代码；只新增输入 seam，不替换所有 Transport 方法）：

```rust
// ProviderAdapter: pure provider-local selection
fn input_contract(&self, ctx: &DeliveryContext) -> Result<InputContract, ProviderError>;

// Provider-local pure callback, carried by InputContract
fn classify_input(sample: &InputSample) -> InputObservation;

// Transport: explicit request; no caller thread-local assumptions
fn inject_with_contract(
    &self,
    request: &InjectionRequest,
    contract: &InputContract,
    observer: Option<&dyn SubmitObserver>,
) -> Result<InjectionReceipt, NoEffectError>;
```

- `DeliveryContext`：收件 Provider、native version、operation/channel、auth、backend；native RPC 在 messaging 分流，不调用该终端方法。
- `InjectionRequest`：目标 endpoint/pane/generation、attempt id、可信 message id（控制命令则明确无）、`Text / ControlCommand / Wake`、渲染 bytes、绝对 deadline。
- `InputSample`：本次身份、前后 capture 的时间/范围、paste latch、执行步骤与 native receipt（如有）。`InputContract` 携带静态纯 classifier 回调，执行器把 sample 交给它；Transport 不查 Provider registry，也不接收有 I/O 能力的 adapter。`classify_input` 不 I/O、不按键；无法定位本次 composer/菜单就 Unknown。
- `InputObservation`：surface（Shell / StartupMenu / Composer / Confirmation / NativeQueue / Executing / Unknown）、本次 identity 的匹配/失配/未知、paste latch 更新及 acceptance 证据。执行器保存本次 latch 并传入下一次 sample；callback 不保全局可变状态。只有当前 surface、identity、步骤都符合规则才能触发下一键。
- `InjectionReceipt`：`effect`、达到的 stage、每类物理计数、观测列表、problem、retry advice。问题发生在可能 paste/submit 之后时返回带问题的 receipt，**不是暗示可安全重投的 NoEffectError**。
- `NoEffectError` 只适用于证明尚无物理 effect 的失败（如前置能力/身份拒绝）。子进程返回错误但可能已执行时必须归 Partial/Unknown effect。

这两个拟议接口及 classifier 回调要连上 `deliver_prepared_message` 的真实 caller 才算接入。默认实现不能 `Ok(empty)`；未迁移 backend 必须明确 Unsupported。旧 `inject` 保留给兼容路径，禁止新 Provider 默默继承 generic 重按策略。

### 5.3 可扩展槽位：数据先行，有条件才加键

`InputContract` 是 provider-local、可审查的配置，不是开放给用户注入任意 shell/key 宏的 YAML。最少包含：

| 槽位 | 允许表达 | 强制约束 |
|---|---|---|
| `framing` | bracketed paste；无 suffix 或已证 native literal suffix | Team envelope 不改；suffix 在同一次 paste 的物理载荷中附加，不伪装 Team token；不自动补 LF |
| `submit` | `SingleEnter`；`ConfirmEnter { additional_enters, confirmation_rule }`；`GuardedRetry { max_retries, retry_rule }` | 用逻辑 `Key::Enter`；确认与重试互斥建模；计数均有限，先准入再 paste |
| `paste_ready` | 本次 token / 新 fold identity / 原生 composer 证据 | 定义 NeverSeen、Seen、Gone；旧 fold 或全文搜索命中不够 |
| `prompt_gate` | 本次 input surface、trust/menu/busy 识别 | shell prompt、native input prompt、确认菜单分别分类；不能仅靠宽泛 `>` |
| `timing` | paste-to-submit floor、各阶段 poll 间隔/超时、最小键间隔 | 单调时钟；poll 算入 floor；budget 耗尽不制造 Ready；sleep 本身不是证据 |
| `queue` | Preserve；或显式 native queue marker 下的有界 Flush | 默认 Preserve；flush 是单独动作和预算，不能把正文中的 send-now 字串当 native UI |
| `acceptance` | 仅 KeySent / compositor consumed / native transcript / protocol receipt 的要求 | 说明证据强度，不能通过改名字升级保证；与业务回程分开 |

单 Enter 示例：`SingleEnter`，额外键 0，queue Preserve，最大 submit Enter=1。首次 Enter 后即使 capture 失败，也只读回，不补键。

双 Enter 确认示例：`ConfirmEnter { additional_enters: 1, confirmation_rule: CurrentMessageConfirmation }`。第一个 Enter 只打开 native 确认；**仅当新出现的确认界面能关联当前 attempt/message，且未开始执行**，才允许第二个 Enter。固定 pause 后无条件 `Enter, Enter` 不合规。确认未出现/身份不明返回 Unknown，不把第二键改称 retry。

多级确认可扩 `additional_enters`，每一步都要对应 native 证据与上界；不是盲发 N 次。准入时核 `additional_enters > 0`、`1 + additional_enters <= max_total_enters`、正 poll 间隔和总预算；运行时全部阶段共用剩余预算。网络或读取 retry 不消耗键预算。queue flush 与 GuardedRetry 不能各自无限循环，必须受同一 `max_total_enters`/deadline 限制。

**规范 C08 — 一次 paste，有限且有原因的键。** 对一次 attempt：

```text
validate binding + capability + budget
  → serialize input for (endpoint, pane, generation)
  → capture baseline / input-surface gate
  → set/load owned buffer → paste once → release owned buffer
  → await current paste evidence AND floor
  → prepare host mode → initial Enter → record physical-submit evidence
  → [current confirmation OR guarded retry, bounded]
  → [explicit queue flush, independently authorized and bounded]
  → observe receipt / return unresolved evidence
```

新协议在输入序列化失败时应 defer，不能无锁继续；现有 200ms 软锁是告警后继续，不能拿它当强互斥。用户手工输入/并发 native 改变导致身份失配时停止自动加键。不得在失败分支重新 paste 或自动 Escape/Ctrl-C 清空 composer；不手工发送孤立 CSI201~。

逐次记录 `paste_count`、`initial_enters`、`confirmation_enters`、`retry_enters`、`queue_flush_enters`、`mode_keys`、`capture_retries`。`total_enters` 是四类 Enter 之和；host mode 的 `q/d/cancel` 单列。现有 `InjectReport.attempts` 混有 capture retries，**不可用它换算回车次数**。

### 5.4 当前 Tmux 行为：兼容区，不是新 Provider 的缺省

现有普通 worker 是 Text+token、bracketed=true：

| 步骤/通道 | 基线事实 |
|---|---|
| buffer | `<16 KiB` set-buffer；`≥16 KiB` load-buffer/stdin；一次 paste 后 delete；不逐字符输入 |
| paste-ready | Tail80，50ms poll，窗口 `max(2000, bytes/25)` ms；timeout/读失败仍可能继续 submit |
| floor | Cursor/Grok caller 设 1s；其余 0；不等于 Transport 自动识别 Provider |
| mode | 每次 Enter 前查询；Copy/Unknown cancel，Tree/View `q`，Client `d`；失败不阻止 submit |
| generic submit | 主循环最多 3 次总提交；特定 wrapped-gap 额外最多 1 次；不代表固定四次 |
| consumption | Tail40，最多 12×100ms；连续 capture 失败最多 4 次、20ms pause；失败只重读 |
| Cursor | 仍有最多 3 的主循环；busy 停止，重按须本次 token 在底部五个非空行且无 busy；无 wrap-gap/flush |
| Pi | 此 caller 仍 generic；“单 Enter/禁 flush”两个 helper 仅定义和测试，未生产接线 |
| Grok queue | `Enter:send now` 存在才额外最多 8 键，50ms 间隔；不是固定 12 键 |
| Claude/Codex/Gemini | generic；普通 caller 也扫描 Grok queue mark，无 mark 才是 0 额外键 |
| human leader | `TextSkipConsumptionPoll` 一次 Enter，但仍需 authoritative leader receipt |
| leader fallback | 裸 Text/inject，不设置 worker Provider TLS，不能套 worker 特例 |
| in-window fork | 无 token、非 bracketed command；首次 1 + 至多 7 重按，不重新 paste |
| Codex app-server leader | Unix socket initialize/resume/binding check/turn-start；**0 个 Tmux 键** |

已有 safe defaults、条件重按、特殊 channel 不应在契约提案中被写成“已全部统一”。迁移必须给上述各 caller 显式选 policy；不能只改一个 helper 的返回值就宣布 Pi/leader 已遵守新标准。

### 5.5 物理提交后的不确定性与回包判定

**规范 C09 — 未知提交不等于未提交。** effect 至少区分 `NoEffect`、`MayHavePasted`、`PastedUnsubmitted`、`MayHaveSubmitted`、`Submitted`；证据另记。操作开始前持久化 attempt intent，恢复后若无法证明 NoEffect，先对账同一 message，不盲目另起 paste。

- token/fold 出现在 composer，只证明输入可见；消失只有在本次曾见过或有更强 receipt 时才有意义。NeverSeen 不能因 absence 被判消费。
- broad busy、spinner、`NewTurnBoundary` 是弱启发式，不是原生 acknowledgement；正文/历史可能含同样文本。
- Claude 已知 authoritative rollout 有本次 token 的额外检查；leader 的 TokenAbsent / SourceUnavailable 保持 pending/unresolved，后续只观察同一 row，不重新注入。
- physical submit 后 observer、日志或 state 保存失败，不撤销已经按下的键。存在本次 token/更强证据时保留提交并 degrade；无法判断则 MayHaveSubmitted，不宣传 exactly-once。
- native RPC 即使有 client message id，也只有 native 协议明确承诺时才可利用其 dedup；`inProgress` 表示接受，不是工作完成。
- 真正业务回包是 owned MCP 对当前消息/turn/task 的持久返回，并检查原始结果语义。`report_result` 的工具 transport success 不自动证明其报告的测试运行过。

终端路径的可承诺范围是“有界提交、抑制已知重复、歧义不自动重投”；**不是跨 CLI/进程崩溃的 exactly-once 执行保证**。

## 6. 三层探针合同

三个层级是不同命题，不是同一个 `ready` bool 的三个别名；可并行采样，但结果必须带同一实例身份和时间，不能跨 restart 拼接旧证据。

| 层级 | 输入、权威事实 | 成功命题 | 禁止替代证据 |
|---|---|---|---|
| L1 Process | 已记录 native PID/进程归属、fresh cohort、wrapper authentic exit receipt、有限 OS probe | 此 native Provider 实例存活 / 已退出 / 不可验证 | pane_pid、inert shell 活着、进程名相似、历史退出文本 |
| L2 Host / Tmux | selected endpoint、returned pane、session/window、generation、owner、capture/query；coordinator 单列 | 目标 pane 可寻址、归属匹配；InputSurfaceReady 另有界面证据 | has-session 即 ready；list 查询失败当空；coordinator 活着当 MCP 可用 |
| L3 Protocol / MCP | owned runtime binding、实际三工具目录、真实工具返回、durable correlation | 当前实例的工具可用或业务回程已验证 | 文件已写、argv 含 config、prompt 列出工具、turn busy、目录里存在模型 |

**规范 C10 — L3 分级且不自锁。**

- `Configured`：预期载体已物化，尚不保证 native 读取。
- `Available`：当前实例拥有的实际 tools/proxy binding 可调用；允许第一条真实任务/授权探测。
- `Returned(tool, call_id)`：指定 owned 工具有实际非 transport-error 返回；仍须读业务 payload。
- `BusinessVerified(message/task/result)`：本次请求、回程、归属和原始业务结果一致；有限验收意义上的通过。

只有需要的工具达到相应证据级别，才可声称该能力 verified。Pi 的 `ToolsVerified` 当前是三个 owned 非 error tool-result 事件的集合，不代表每个业务 payload 成功。

不能要求 `BusinessVerified` 之后才能发第一条任务，否则形成循环等待。也不能让 doctor 为“探针”自动 `report_result` 终结真实任务，或循环 `send_message` 制造消息。优先只读 `get_team_status`；发送/结果回程在授权隔离验收任务中完成。没有原生 tool registry 观察接口时，L3 保持 Unknown，允许显式批准的有限验收投递以建立回程证据，不自动改名 Available。

### 6.1 预算、 freshness 与失败政策

- 每层记录 `source, subject_identity, observed_at, elapsed, deadline, outcome, reason`；deadline 用单调时钟，剩余预算传到子 probe，不能嵌套重置延长。
- timeout 表示观察窗口耗尽，不等 Dead/Idle/Ready。IO 错误不删除旧资源；只可终止本次创建的 probe child，不能因 readiness timeout 杀目标 Provider。
- Available/活性证据随 generation、binding、endpoint、native session 改变失效；工具被 unregister 或绑定消失必须降级。旧 Verified 留作历史收据，不能覆盖当前 unhealthy。
- `status_patterns` 的定义不等实际 tick 使用。当前 tick 是 JSONL-first，再走 generic activity；无 Provider semantic reader 则明确 Unknown/弱推断，不以长静默自动 Idle。
- 未闭后台任务使 Claude end_turn 不足以判 Idle；前台 PGID 不可读不视同空闲。输入 gate、调度空闲判定、破坏性清场有不同安全政策，不能共用一个 false fallback。

当前预算只作兼容事实：spawn identity 500ms/25ms；restart convergence 默认 12s/250ms；restart infra readiness 默认 30s/200ms；Pi runtime binding 30s/25ms；OS probe 默认 900ms/10ms；tmux runner nominal 5s。**这不是统一硬 deadline**，部分 stdin/join/OS 边界仍不能证明绝对上限。新 Provider 必须给每个新增等待一个可测试预算，不能把这些数值当新标准的全球常量。

当前 quick-start 的 worker 汇总通常为 `PendingToolLoad`；restart readiness 仅 `session_created ∧ pane_addressable ∧ coordinator_alive`。兼容输出不改名冒充 L3；未来报告可以增补分层 evidence，但旧 Ready 字段不能静默换语义。

## 7. 会话、Resume 与 Fork 合同

### 7.1 不同对象必须不同名字

**规范 C11 — fork 按模式准入，不从 `caps.fork` 推导。**

| 模式 | 新对象 / 身份 | 成功证明 | 不能承诺 |
|---|---|---|---|
| `NativeForkPlan` | argv/准备计划，尚未启动 | native 参数及必要 backing 物化正确 | 公开 `fork-agent` 可达、Agent 已可用 |
| `InWindowBranch` | 同 agent、同 pane；native 分支 | 本次命令之后的新 native 分支证据 | 新 MCP 身份、新席位、可恢复的新 session id |
| `NewSeatFullSnapshot` | 不同 agent/native session、独立 pane 和 Team identity | 完整 backing 快照事务 + 注册/启动收据；readiness 另列 | 仅屏幕标记即可证明、工具已可用 |
| `ContextClone` | 有限上下文/配置副本 | clone 范围与来源明确 | full-session fork 或等价历史 |

拟议 typed admission/result 放在公共 fork orchestration，复用现有 adapter native plan；不把旧 bool 增殖成四个新 bool。至少返回 `mode, source_identity, target_identity, evidence_origin, backing_state, readiness, effects/preserved`。InWindowBranch 的 `target_agent_id=source_agent_id`、`new_session_id=None` 是合法结果，不制造 UUID。

当前公开矩阵：Claude subscription `/branch` + `Branched conversation`、Grok subscription `/fork` + `forked from` 是原窗模式；Pi subscription 是不同目标席完整快照；Codex/Gemini in-window 未验证拒绝，Cursor 不支持。Claude/Grok 拒 distinct `--as`，Pi 要求 distinct target。native adapter 的 auth 门与公开门不同，不顺手拉齐。

### 7.2 Resume binding

resume 必须依赖 captured sid、backing path/root、cwd、source generation、attribution confidence 的有效组合；有 sid 但没 authoritative backing 不等于可恢复。exact-path Provider 不得降为 sid 搜索；source 已变、新实例替换或会话缺失时返回 ResumeUnavailable，除非用户另行显式允许 fresh。

会话候选扫描保留多候选/歧义，让 shared allocator 归属；不把同 cwd 最新文件直接分给所有 Agent。native 构造参数、capture、restart postflight 都需测试；只过启动前检查不证明 native 真恢复了期望 session。

### 7.3 Full snapshot 事务

以当前 Pi 路径为约束来源，而不是要求所有 Provider 的 backing 都是 Pi JSONL：

1. **F0 preflight**：锁定 selected team/source，验证 auth、source/target path component、target distinct、无 role/spec/state/routing/pane 冲突，source 具有高置信 exact binding。
2. **F1 snapshot**：读取最新角色与 cwd；防 symlink/非 regular file；稳定读取完整源，核 metadata/path/bytes、格式、header id/cwd、记录链。只复制 tail 或屏幕不合格。
3. **F2 stage/register**：新 native id 与 ancestry；按该 Provider 格式重写允许的 header，保持正文记录；只注册本 target 的 spec/route/state，compare-own-bytes 防丢并发字段。
4. **F3 spawn**：重新核 source/target，shared materializer exact resume → shared worker wrapper；记录自己创建的 pane，复核 endpoint/session/window。
5. **F4 commit**：保存 target resume tuple、coordinator 与健康状态；写事务/audit 收据。backing Verified 不自动设置 L3 ready。
6. **F5 compensate**：仅处理本次 target；先证明无 writer，再杀已证 owned 新 pane；文件仍等 staged bytes、row/spec 仍属于本操作才删除/回退。

source 在读取中变化、未知 writer、新记录追加、目标被并发改变时保留资源和角色，报告 preserved/partial，不删除新上下文、不恢复整个旧 workspace snapshot。每个阶段注入故障都要证明 source、leader、sibling/coordinator 不受误伤。

InWindowBranch 没有等价回滚机制。新契约要分辨旧 screen mark 与本次新证据；键已发但验收/审计失败，应报告 Unknown/Partial branch，而不是自动再 paste slash。unsupported/unverified 分支必须在任何按键或新 pane 前拒绝。

## 8. 停止、移除与资源清场

**规范 C12 — owned 不等于 exclusive，失败不等于零 effects。** Provider 只声明自己创建的资源，真正 stop/reset/remove/shutdown 由共享 lifecycle 做归属和残留判断。

- stop 保会话 backing；reset 必须 discard-session 并检查旧实例残留；remove 只移除目标注册和可证 owned role/wrapper，不删除用户外部 role、session 历史或 HOME 设置。
- 破坏动作前 fresh 验证 endpoint + generation + owner；精确 pane/PID。进程名、裸 cwd、父进程继承环境不是单独的销毁授权。
- TERM/等待/再探测/必要时 KILL 仍须当前 identity 匹配、范围正确、audit 成功。查询失败不升级暴力清理；禁止 `pkill/killall/kill-server` 或广域 socket 删除。
- 基线 scoped shutdown 先做已证 per-PID effects，再过 session ownership gate。后者 Unknown 拒绝，不代表前面无 effects；不要把整个过程包装成原子失败。
- leader authentic receipt、caller/ancestor/PGID、live sibling、共享 coordinator 必须保护。即使 owned 空 endpoint 也不删 server/socket；不能为“残留为零”越权。
- 原生 Provider global config、用户 transcript、Pi fork 已追加记录的 backing 不属于常规清场可任意删除的资源。

清场收据至少有：`scope, effects_attempted, effects_observed, spared, preserved, residuals, verification_errors, outcome`。outcome 区分 clean/partial/dirty/timeout/unknown；fresh probe 失败不能发 clean。停止命令返回、行政状态 stopped、physical cleanup 完成是三个事实。

## 9. 手把手接入第七家：Nova CLI（虚构示例）

**Nova 及以下 native flags/协议全部是假设，不是现有产品，也不在当前 Team Agent 可用。** 实际接入先用合法官方帮助/文档和有限实机确认替换假设；不通过猜 flag 完成开发。

### 第 0 步：写一张可证的 native 合同卡

示例冻结的假设：

| 项 | Nova 的明确选择 |
|---|---|
| 身份/范围 | wire `nova_cli`，binary `nova`；第一版仅 subscription worker + Tmux；无 native leader verb |
| catalog/model | `nova models --json` 返回完整 JSON；ID 原文；无 model 用 native default，有值必须 exact catalog member |
| effort/permission | low/medium/high/xhigh 原档 `--effort`；max/ultra 拒绝；仅显式 bypass=true 加 `--yes` |
| prompt/MCP | `--system-prompt TEXT`，`--mcp-config FILE`；seat-scoped 文件；官方可观察当前三工具 binding |
| 会话 | 官方 `--session-id SID`、`--resume SID`；可设置 seat root，archive header 含 sid/cwd；不支持 fork |
| 收件 | bracketed paste；无 native suffix；本次消息确认 UI 在第一个 Enter 后出现，带本次 marker；第二个 Enter 才提交 |
| 观测 | 确认 UI 可与 baseline 区别，accepted transcript 可关联 message；无 semantic turn reader 时 activity Unknown |
| 预算 | 示例 floor=250ms、paste-ready=2s、confirmation=2s、acceptance=5s、poll=50ms、total=10s；均待 native 证据验证 |

任何假设无法证明就降级该能力/停止该条路径；不能把虚构预算写成经过调优的通用值。首次不支持 fork 是合格边界，不是需要假实现的缺口。

### 第 1 步：注册与准入，不先启动

1. `model/enums.rs` 增 `NovaCli`，`provider/wire.rs` 增 canonical/binary/显式 alias 和穷尽映射；检查所有 enum consumer 的编译缺口，未知值不得 fallback 成其他 Provider。
2. compiler、direct spec、role creation/runtime patch 的 provider/auth/effort 准入一致表达 Nova 合同；保现有入口的错误层次、raw bytes 与默认值。审查 schema/公开帮助，但不擅自增加 `team-agent nova` 命令。
3. 更新 catalog source/parser/action，保有界 runner 和 shared matcher；原始 fixtures 覆盖完整格式、重复、顺序、no-match、坏输出、超限、timeout。
4. 模型解析与 effort resolver 分开；同一请求从 role、direct spec、add/start/restart 均不能绕过准入。

### 第 2 步：最小 argv + 有副作用物化

- 新建 `provider/adapters/nova.rs`：native argv、输入 policy 数据和纯识别函数；在现有 `BasicProviderAdapter` match 中接线，复用 `append_pair/append_opt_pair`。
- `CommandPlan` 标注 expected sid 与 seat root；必要的新 `lifecycle/launch/nova_mcp.rs` 仅负责 Nova scoped config/root 的物化及 runtime binding 观察（**拟议文件**），不包含第二套 spawn。
- `spawn_agents`、`spawn_agent_window` 接相同 materializer；fresh/resume/失败补偿均记录自有资源。没声明 leader 支持就不要改 `leader/start.rs` 公开面。
- fake native/shim 检查 exact ordered argv、bypass true/false、unset 最终胜出、prompt 引号/LF、MCP file 的实际读取、ambient leader identity 被剥离。

### 第 3 步：把双 Enter 接到真正消息链

Nova 的 provider-local policy（伪配置，不是当前 schema 字段）：

```text
channel           = WorkerTmux
framing           = BracketedPaste(suffix = None)
submit            = ConfirmEnter(additional_enters = 1,
                                 rule = CurrentMessageConfirmation)
retry_enters      = 0
queue             = Preserve
max_total_enters  = 2
acceptance        = NativeTranscriptForCurrentMessage
```

在 `deliver_prepared_message` 选择 Nova contract，并把可信 message id/attempt/binding 显式传入拟议 Transport seam。确认收到第二键后，observer 只能记录对应物理步骤，不能伪造工具回程。未知 channel 返回 Unsupported；Wake 单独声明不消费业务消息；不要把双确认套到 slash/trust/leader。

测试 oracle：准确一次 paste；确认前恰好一个 Enter；当前确认出现后总共两个；历史确认、wrong marker、busy、capture failure、deadline 到期均不产生第二键；后续 observer failure 不重新 paste。

### 第 4 步：三层探针与会话恢复

接 shared L1/L2，不新增 provider 进程扫描器。Nova scoped binding observer 为 L3 提供来源和 generation；通过授权任务分别看到三工具实际返回。把 capture/archive parser 接 `capture_session_candidates`，用隔离同 cwd 双席验证不串 session；resume 后检查实际 sid/cwd/backing，不仅检查 argv。

fork 第一版在公共入口明确 Unsupported，断言 0 keys、0 new pane、source 未变。将来只有 native/完整 snapshot 证据到位，才另行声明模式并运行 §7 的故障矩阵。

### 第 5 步：编译、自动化与有限真实验收

先在被授权构建环境完成 compile 与针对性 tests，不能把 fake transport 测试写成 native PASS。每个 fixture 还要通过生产 caller；helper 绿但接线没用视为未完成。

接入完成之后的示例 role（当前黄金基线会拒绝这个 Provider）：

```yaml
---
agent_id: nova-worker
role: provider-contract-smoke
provider: nova_cli
auth_mode: subscription
dangerously_skip_permissions: true
# model/effort 可省略；如填写，必须符合本次 native 合同。
---
仅执行本次隔离验收任务。使用实际绑定的 Team 工具回程，不自报 sender 或 task 身份。
```

`true` 仅用于已明确授权 bypass 的隔离环境；普通部署按用户权限选择，不默认扩权。通过公开 [操作指南](reference/team-agent-operator.md) 建立完整隔离 TEAM/role 配置，固定 binary 绝对路径、source/hash、native CLI 版本、model、backend/endpoint 和 team scope。下列是有限用例设计，不授权在正在运行的用户 Team 上直接执行：

1. `models --provider nova_cli --json`：原生目录原样可选；坏源失败；`quick-start` 或 `add-agent --role-file`：L1/L2 与 L3 各自留证。
2. `send` 发唯一验收任务：等待自然 `send_message` 和一次 `report_result` 的原始 durable 结果，核 identity/归属/通知；不能只看 queued。观察窗口预先固定。
3. scoped `shutdown --keep-logs`，再 `restart` 恢复同会话；再次自然回程；调用公开 fork 应安全拒绝。**不加 `--allow-fresh` 掩盖 resume 失败。**
4. 再次 scoped shutdown，复验该 scope 的 owned 残留及 leader/sibling 保护；记录保留 backing/socket 的原因。

命令参数以候选 `--help` 和公开指南为准，`nova_cli` 注册前不运行示例。实机 caller/被测模型遵守项目授权，不换更强模型、手动按键或重复发送补救来凑通过。首个意外错误停用例、保现场；修复后用干净 scope 重跑同一完整用例。没有允许的模型/平台/订阅则记录 NOT-RUN/UNKNOWN，不伪造认证。

## 10. 验收矩阵与接入 Check List

下表是未来实现的有限验收要求，**本设计 PR 未执行这些测试**。复用已有 test harness；测试文件存在不等于它已通过。

| 编号 | 场景 / 必须能反驳的错误 | oracle |
|---|---|---|
| T01 | wire/alias/grammar；把 unknown 当 Codex | canonical round-trip；只准入声明入口；unsupported 无危险 effects |
| T02 | catalog/model/effort；trim/sort/模糊选模型/降档 | 原 ID/顺序/错误全文；六档决策；no-match 与 source failure 分开 |
| T03 | argv/env/profile/prompt；scope 或 unset 漏洞 | exact ordered vectors、真实 file carrier、身份隔离；无 secret 日志 |
| T04 | cold/dynamic/resume/fork target 接线不同 | 同一有效请求走同一 Provider 决策；host policy 仍按 operation 不同 |
| T05 | single Enter / dual confirmation | 精确键序与时序；一次 paste；第二键仅当前确认；控制命令不混入 |
| T06 | embedded token、Unicode、多行、CR、16 KiB 边界 | 正文保真、可信 message id 显式匹配、buffer 分流正确、不把 LF 当键 |
| T07 | old fold/confirmation、正文 busy/send-now、never-seen | 不错认消费/确认/queue；unknown 不补键、不选错 message |
| T08 | capture error、锁冲突、deadline、endpoint/generation 改变 | bounded poll；0 额外 Enter；错误时保未知；错 target 不写入 |
| T09 | paste/submit 后 crash、observer/save/receipt failure | effect 记录不倒退；同 message 对账，不第二次 paste；不得声称 exactly-once |
| T10 | human leader/fallback/native RPC | 各 channel 明确 policy；RPC 0 keys；leader receipt 不借 worker token 代替 |
| T11 | wrapper 活、Provider 退出、历史 marker、旧 MCP binding | L1/L2/L3 不串层；generation 失效；unknown 不归 Idle/Ready |
| T12 | tool registry 有但调用 error；result ok 通知 queued | Available/Returned/业务结果/leader显示分开；三工具原始返回各自查验 |
| T13 | same-cwd 两席、缺 backing、sid-only 假恢复 | 强归属；歧义拒绝；无授权 fresh fallback；postflight 验 session |
| T14 | 原窗 branch / Unsupported fork | 新证据不同于旧 mark；原窗不造新 id；拒绝路径 0 keys/0 pane |
| T15 | full snapshot 在 F0–F5 故障或 concurrent writer | source/sibling 不变；只回退 own bytes；有新 records 则 preserved |
| T16 | stop/remove/shutdown ownership unknown、probe failure | 精确 scope；已发生 effects 照实；共享 endpoint/leader 不删；clean 有 fresh proof |

可复用导航：`tests/provider_effort_mapping_contract.rs`、`tests/model_fuzzy_lookup_red.rs`、`tests/provider_submit_verification_red.rs`、`tests/inject_exits_copy_mode_before_submit.rs`、`tests/fork_agent_five_shapes_red.rs`、`tests/mcp_3_tools_red.rs`（均相对 `crates/team-agent/`）。新增原生协议 fixtures 应进入对应 Provider 测试模块，不读取真实 credential/profile/session 内容来制造公开 fixture。

### 必选清单

- [ ] 能力卡逐项写 Declared/Reachable/Observed/Verified、auth/native-version/platform/channel 与未知项。
- [ ] 所有新入口在副作用前准入；provider alias 与 executable/model alias 分离。
- [ ] model discovery、exact/opaque selection、effort 六档、权限和 profile 分别有依据。
- [ ] raw argv 与物化分离；共享 env/unset、identity、host wrapper/route 没被绕过。
- [ ] MCP carrier 被 native 实际读取；工具名来自真实 binding；三 logical tools 不另造协议。
- [ ] 物理 policy 在生产 caller 使用；一次 paste；1/2/多级 Enter 有明确 guard、预算和真实计数。
- [ ] token/framing/suffix 与 submit key、native receipt、business end 分开；碰撞 fixture 覆盖。
- [ ] 读失败、busy、旧 UI、提交后 observer 失败不会触发盲重发；Partial/Unknown 可对账。
- [ ] L1/L2/L3 独立，freshness/timeout 与 current identity 可审计；业务探针不自动终结真实任务。
- [ ] resume 强归属；fork 模式、auth 门和 unsupported 拒绝都有反例测试。
- [ ] 有明确 resource scope/补偿；owned writer/并发改变时保留；清场有限且复验。
- [ ] 自动化实际执行；有限 native 自然回程完成；source/binary/native-version 与收据对应。
- [ ] 未执行、unknown、unsupported、partial 单列；文档/帮助/catalog 注册与实现一致。

## 11. 六家兼容迁移与实施顺序

本提案不批准一次性重构六家，也不因现状未知开启额外缺陷修复。批准实施后建议按风险推进：

1. **先保行为**：把 §5.4 各真实 caller 的 argv/keys/counters/结果边界作 characterization；包括尚未生产接线的 Pi helpers、裸 leader fallback、Gemini 部分接入，不能拿 helper 测试替整链。
2. **新 seam opt-in**：实现显式 request/policy/receipt；新 Provider 必须显式声明；现有六家保留兼容路径。版本标识留在诊断/contract 内部，不新造公共 schema 字段。
3. **一条 channel 一次迁移**：由旧 TLS/heuristic 转显式 policy 前，对照相同输入、按键序、marker、timing 与错误语义；terminal/RPC、worker/leader 分开。
4. **行为变更单独裁决**：first-token 解析改显式 identity、软锁改 defer、paste-ready timeout 改不按键、队列仅 native UI、Pi 真单 Enter、fresh screen mark、probe 更强 gate，都是语义变化，不能打包成“纯重构”。
5. **证据跟变更失效**：文档或 test-only 变化不要求重测所有订阅；改物理 bytes、身份、资源归属的路径需要对应有限实机。已有 Pi/Codex launch→回程→shutdown→resume→回程证据不能扩大为六家/fork/Windows 全通过。

最终接入的完成标准不是“trait 编译了”或“有一个绿色 PR”，而是：**声明的公开操作可达，有限真实任务能回程，未知边界被保留，失败不重复执行或越权清场。**
