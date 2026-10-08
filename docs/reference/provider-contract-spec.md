# 新 Provider 适配标准契约规范与接入指南

**New Provider Adaptation Contract & Specification — 最终统合设计提案 v2**

综合 [Opus 独立方案 #299 / `1b8a6cfd`](https://github.com/Florious95/team-agent/pull/299) 与 [Astra 独立方案 #300 / `0f6101a7`](https://github.com/Florious95/team-agent/pull/300)。本版是提交用户检阅的统合设计，**不是已批准实施的运行时规范**。

- 状态：**设计提案，尚未实施，也不是六家 Provider 的实机认证**。
- 事实基线：[`8ff50cfa21c868ff08bf27a36ed8447154b69cc5`](https://github.com/Florious95/team-agent/tree/8ff50cfa21c868ff08bf27a36ed8447154b69cc5)，tree `dfef23ca1611faffbdb10bcf253ecfe627139c82`，产品版本 `0.5.112`。
- **现状**指该基线的函数体及生产调用；**规范**指新接入必须满足的目标；**拟议 API**均尚不存在。不得把后两者当成当前 API 使用。
- 范围：模型发现到停止清场的完整接入；六家现状兼容边界；第七家虚构 Provider 的教程。不改产品、提示词、公开命令集合或持久格式，不授权迁移、发布、全局配置修改。
- 规范用语：MUST / MUST NOT / SHOULD / MAY 分别为必须 / 禁止 / 建议 / 可选；中文“规范 Cxx”对**新契约实现**生效。基线不满足项见 §12 D-01–D-22，不自动成为修复任务。
- 唯一公开入口为本文件；原 `docs/provider-contract-spec.md` 已移入 `docs/reference/`，避免两份规范漂移。类型/签名均为设计草图，不是可复制编译的 API。
- 阅读路线：§1 描述符与七窄钩子 → §2 生命周期 → §3 函数地图 → §4 准入/物化 → §5 物理协议 → §6 探针 → §7 Fork → §8 清场 → §9–10 教程/验收 → §11–12 迁移/偏差。

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

### 1.4 编译期全量描述符：数据与行为正交

采纳 `ProviderDescriptor` 作为目标单一事实入口。它是编译期 Rust 数据，不是外部 YAML 插件；每个 facet 必填，能力缺失必须有原因，不以 `_ => generic`、`Default` 或无说明的 `None` 补齐。

```rust
// Target only: declarative data; no closures, I/O, secrets or observed readiness.
struct ProviderDescriptor {
    identity: Identity,              // wire, aliases, binary, explicit leader surface
    model: ModelPolicy,               // omission, trim/shape, opaque/exact, catalog route
    effort: EffortPolicy,             // all six admissions, carrier, inheritance
    auth: AuthPolicy,                 // per-auth support and existing mapper identity
    bypass: BypassPolicy,             // explicit true/false behavior or NotCarried
    prompt: PromptCarrier,
    mcp: McpCarrier,                   // carrier + scope; not a supports_mcp bool
    tool_names: ToolNamingPolicy,     // verified native convention or runtime binding
    session: SessionPolicy,           // id allocation, resume binding, backing format
    fork: ForkPolicy,                 // public modes by auth/surface, not native argv alone
    input: InputProfile,              // literal data resolved with operation × channel
    startup: StartupPolicy,           // modal shapes/action ids; no automatic consent
    probe_sources: ProbeSources,
    workspace: WorkspacePolicy,       // isolated root, cwd exclusivity, native restrictions
    teardown: TeardownPolicy,         // removable / preserved / shared / forbidden
}
enum Support<T> { Supported(T), Unsupported(Reason), Unverified(Reason) }
```

关键 facet 的最小表达：

| Facet | 必须能表达 | 不能从中推断 |
|---|---|---|
| Identity | canonical wire、parse-only aliases、binary、leader surface Unsupported/Supported | 增 enum 自动开放所有 CLI grammar |
| Model | NativeDefault、OpaqueId 或 ExactCatalogId；各入口 raw/trim/shape | catalog 搜索结果自动变 launch id；统一 exactly-one-slash |
| Effort | `[Admission; 6]`，Pass / IgnoreWithEvent / Reject；native carrier 与 TEAM 继承 | 每模型实际支持、native client delegation 等同 API effort |
| Prompt | AppendFlag / RulesFlag / ConfigValue / ScopedFile / RuntimeBridge / NotCarried | 计划含路径即 native 已读取 |
| MCP | InlineOrFile / ConfigFlags / DirOverlay / ProjectEnable / RuntimeBridge / GlobalInstaller / Unsupported | flags=false 即无 MCP；global writes 已获授权 |
| Startup | RecognizedModals / NoKnownModals / Unverified；scope/consent 前提 | 没有 StartupHook 即屏幕 ready |
| Probe sources | 各层来源、freshness、budget、是否 lazy；不支持有原因 | 来源存在即当前观测成功 |
| Workspace / teardown | invocation/seat/cwd/global scope；独占限制与资源类别 | owned 即 exclusive；同 cwd 即可杀 |

描述符只存**声明的事实/政策**；native version、观察证据和已达成熟度属于 `AcceptanceRecord`/运行时 probe，不放入静态 `maturity=L4` 字段。相同 Provider 可以某 auth/版本的 resume 已验收，而另一 channel 未执行，不能用“一个最高等级”抹掉差别。

目标注册表对每个 `Provider` 穷尽返回 descriptor 与显式 hook bindings；已有六家可以先 mirror 保行为，不立即迁移。编译器只能证明字段/分支全量，不能证明 caller 已接线、hooks 正确或 native 支持，仍需 T04/T17。

### 1.5 七个 Narrow Hooks：只有行为进入钩子

`ProviderHooks` 与 descriptor 并列绑定，不能把回调藏进 descriptor。每个 binding 明确为 Bound / NotRequired(reason) / Unsupported(reason) / Unverified(reason)；**NotRequired 仅限本次操作确无该需求**，不是缺实现的成功出口。

| Hook | 目标接口/产物（示意） | 纯度与条件 |
|---|---|---|
| H1 `CatalogHook` | `discover(&BoundedRunner) -> Result<Vec<ModelRecord>, CatalogError>` | 可选、有界 native I/O；若声明支持 catalog 则必须绑定；不能静态 fallback |
| H2 `PlanHook` | `plan(&ResolvedLaunch) -> Result<LaunchPlan, PlanError>` | 接入必选；纯函数；fresh/resume/native fork plan 共用唯一 argv assembler |
| H3 `MaterializeHook` | `apply(&MaterializationPlan, &OwnedIo) -> MaterializationOutcome` | 有文件/extension/overlay 需求时必选；结果含已发生 effects，失败也不丢 receipt |
| H4 `SessionHook` | `capture(..) -> CandidateEvidence`、`probe_backing(..) -> Probe<ResumeBinding>` | resume 声明支持则必选；有界只读；不另拼 argv，输出 binding 给 H2 |
| H5 `SemanticReader` | `classify(&TranscriptSlice) -> TurnObservation`、`latest_fault(..)` | 可选纯解析；没有 reader 是 Unsupported，不返回假 Idle/无故障成功 |
| H6 `InteractionHook` | `observe(&InputSample) -> SurfaceObservation` | 纯 UI 解析；统一承载原 StartupHook 与运行期 composer/confirmation/queue 识别，不按键 |
| H7 `ForkHook` | `stage(&VerifiedSource, &OwnedIo) -> ForkStageOutcome`、`compensation_constraints(..) -> CleanupPlan` | 有 provider-specific snapshot/materialization 时必选；只处理声明资源，不直接 spawn/kill/paste |

H6 是对 Opus `StartupHook` 与 Astra `classify_input` 的合并，不再增加第八个大 hook：同一 native UI 格式由一个模块解析，但 startup、composer、confirmation、queue 输出各自的 typed fact。动作由 descriptor 的 action/rule 数据和 shared policy reducer 选择，Hook 不能返回任意可执行 shell。

共同约束：

- H2 不读文件、不起子进程、不看时钟、不生成随机数。session id 由 framework **在调用前**按已声明策略分配并持久化，再作为已解析值传入；不能以“注入随机 SessionIdSource”把副作用藏回纯 plan。
- H3/H7 使用受限资源集合，区分 `Applied(receipt)`、`NotRequired(reason)`、`Failed { partial_receipt, error }`。资源已写后报错不能返回空 receipt；补偿由共享 lifecycle fresh-check 后执行。
- H4 产生已验证 resume binding，H2 产生 argv，避免 `SessionHook::resume_plan` 与 PlanHook 两份命令真相。NativeForkPlan 若仅纯 argv，也由 H2 处理；不要强造 H7。
- H6 的 startup policy 可以声明 `[Down, Enter]` 等 native action；只有当前菜单、授权 scope、选择项都匹配才由 Transport 执行。识别登录/telemetry/信任菜单不等获得自动同意权限。
- Hooks 不操作 tmux、不发键、不控制宿主拓扑、不读取 credential store；profile 凭据映射留在既有受控配置通路，绝不进入诊断/receipt/示例正文。
- 条件能力缺少必要 hook 是注册/准入错误：例如 ScopedFile prompt 没 H3，resume 没 H4，双确认没可靠 H6，都不能靠默认空返回通过。

### 1.6 支持成熟度：证据记录，不是静态属性

| 里程碑 | 最小有限证据 |
|---|---|
| L0 Registered | descriptor/registry 全量、wire/alias/入口准入齐全 |
| L1 Launchable | argv golden + 对应实例 native process 的实际启动证据 |
| L2 Instructable | prompt 的真实 native carrier + 按声明输入契约提交到目标 |
| L3 Round-trip | 三 logical tools 的必要可用/返回证据；当前验收任务的自然回程及原始 payload |
| L4 Durable | authoritative backing resume 后第二次自然回程，scoped shutdown 残留复验 |
| F Forkable（独立 facet） | 某一种公开 fork 模式的明确 live 证据、失败/补偿边界；不要求所有 Provider 必须 fork |

记录键包含 source/binary/native-version/model/auth/platform/operation/channel；为每条记录保留 PASS、FAIL、NOT-RUN、UNKNOWN 或能力拒绝，不以静态枚举写“已认证”。黄金基线只有 Pi/Codex 有有限 L4 路径的既有收据；不覆盖 fork/所有模型/Windows。Claude/Cursor/Grok 在此基线未重新验证；Gemini 仅有 launch 声明/代码可达，**不能在未实机时授予已达 L1**。

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

Hook 路由唯一化：P2→H1，P3→H2，P4→H3，P6/P8 的 native UI→H6，P10→H4/H5；fork stage→H7。L1/L2 的 OS/host 观察、MCP server 观察、persist/notify/teardown 仍由共享层执行，不变成每家重复的第八至第十个 hook。所有 stage 的 typed 产物向后传递，不能重新从字符串猜先前的 token/session/owner。

目标正常入口先确定 profile 参与的有效 model，再按声明决定 discovery/membership；不得先准入一个 model，随后被 profile 替换成未准入的值。基线各入口的 raw/trim/读取顺序照实保留，改变它们需独立兼容决策。

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
| canonical provider、alias、binary | 现有 [`model/enums.rs`](../../crates/team-agent/src/model/enums.rs) `Provider`；[`provider/wire.rs`](../../crates/team-agent/src/provider/wire.rs) `provider_wire/parse_provider/parse_canonical_provider/command_name` | 分开 canonical spec 与历史 alias；不顺手扩公开 leader verb |
| role/spec、CLI 准入 | 现有 [`compiler.rs`](../../crates/team-agent/src/compiler.rs) `compile_role_agent/compile_team`；[`model/spec.rs`](../../crates/team-agent/src/model/spec.rs) `validate_spec`；`cli/adapters.rs` | 正常入口与 direct spec 都校验；更新适用 schema/帮助文本，不用 fallback Codex 接住漏项 |
| 模型目录与搜索 | 现有 [`provider/model_catalog.rs`](../../crates/team-agent/src/provider/model_catalog.rs) `catalog_source/discover_model_catalog/model_matches`；`cli/models.rs` | native source/parser 独立；共享 bounded runner/search；支持与不支持都写显式分支 |
| Effort | 现有 `ProviderEffort::parse/resolve_for_provider`；compiler、launch/restart caller | 通过 / known-ignore + warning / 拒绝三态；声明继承规则和 native flag，不改旧错误全文 |
| profile 与认证 | 现有 [`lifecycle/profile_launch.rs`](../../crates/team-agent/src/lifecycle/profile_launch.rs) `prepare_provider_profile_launch_with_profile_dir/prepare_provider_profile_launch_from_json/provider_env_exports/provider_env_unsets/provider_command_overrides` | 实现真实 auth mapper；被 spec 接受不是 BYOK 已实现 |
| 身份/prompt/环境 | 现有 `lifecycle/worker_command_context.rs` `from_yaml/from_json/compile_worker_system_prompt`；`lifecycle/launch/worker_env.rs` `inherited_env_with_team_overrides/apply_profile_launch_env` | 消费编译后的 prompt；保 identity/MCP/communication/body/output 顺序与最终隔离 |
| fresh / resume argv | 现有 [`provider/adapter.rs`](../../crates/team-agent/src/provider/adapter.rs) `ProviderAdapter::build_command_plan/build_resume_command_plan`；`provider/adapters/*.rs` | 在 `BasicProviderAdapter` dispatch 增分支，provider-local 函数造 raw argv；特殊 exact-path 用专属物化入口 |
| 命令结构 / shared append | 现有 [`provider/types.rs`](../../crates/team-agent/src/provider/types.rs) `ProviderCommandContext/CommandPlan`；`provider/command_helpers.rs` | 复用原始 pair/optional append，不在 helper 加 trim、escape、排序、去重 |
| MCP 描述/文件 | 现有 `ProviderAdapter::mcp_config/install_mcp`；`lifecycle/launch/mcp_config.rs::resolve_mcp_config` | 给出载体和真实读取路径；installer 定义不算 wired；有副作用安装在 lifecycle |
| runtime MCP bridge | 现有 `lifecycle/launch/pi_mcp.rs::materialize_pi_plan/materialize_pi_resume_plan`；`pi_mcp_extension.js`（仅 Pi） | 新 Provider 若需 bridge，做自己有官方接口的窄物化/观察模块，不冒用 Pi extension |
| 实际 launch 入口 | 现有 [`lifecycle/launch/spawn.rs`](../../crates/team-agent/src/lifecycle/launch/spawn.rs) `spawn_agents`；`lifecycle/restart/common.rs::spawn_agent_window`；`leader/start.rs` | 三条入口显式使用同一配置决策；辅助 catalog/version/MCP enable 不经过 host argv-route |
| 宿主 shell | 现有 [`tmux_backend.rs`](../../crates/team-agent/src/tmux_backend.rs) `shell_command/worker_shell_wrapper_command/leader_shell_wrapper_command/spawn_with_command` | 通常不改；接 operation policy，不塞 Provider if/else |
| 启动菜单/fast-mode | 现有 `provider/startup_prompt.rs`；`ProviderAdapter::handle_startup_prompts_outcome/enable_fast_mode`；`spawn_agents` | H6 识别、descriptor 给动作；Codex `/fast` 当前只有 cold caller，不能据方法存在说 restart 同样执行 |
| Agent → Team | 现有 [`mcp_server/wire.rs`](../../crates/team-agent/src/mcp_server/wire.rs) `dispatch_tool`；`mcp_server/tools.rs::send_message_with_presentation/report_result_with_presentation` | native 工具名映射到现有三 logical tools；身份由框架注入 |
| Team → Agent | 现有 [`messaging/delivery.rs`](../../crates/team-agent/src/messaging/delivery.rs) `deliver_pending_messages/deliver_prepared_message/render_message` | 复用 persist-first funnel，按收件 channel 选协议；不得另建 provider send 队列 |
| 声明/七个行为钩子 | **拟议** `ProviderDescriptor` / `ProviderHooks`；基线仍是 `BasicProviderAdapter` match | 用 §1.4–1.5 做完整 registration；现有 `adapters/<new>.rs` 可继续承载实现，不要求七个新文件 |
| 输入策略/识别 | **拟议** `resolve_submit_policy(operation, channel, descriptor.input, hooks.interaction)`；H6 `observe` | 数据和纯行为分开；结果显式交给 Transport，不新增 ambient TLS |
| 消息级发送 seam | **拟议** `messaging::delivery::deliver_envelope`，见 §5 | 统一 binding/intent/typed marker/receipt 边界；调用下面的唯一物理执行器，不复制按键循环 |
| 契约式物理执行 | 现有 [`transport.rs`](../../crates/team-agent/src/transport.rs) `inject/inject_with_submit_observer/send_keys/capture`；**拟议** `Transport::inject_with_contract` | 显式传 resolved policy/identity；Tmux 一次 paste + 有界键，其他 backend 不支持时拒绝，不静默降级旧 inject |
| native 无键投递 | 现有 `codex_app_server.rs::submit_to_bound_thread/turn_start`；`delivery.rs::deliver_leader_via_app_server` | 绑定 socket/thread 元组；不是通用 CLI worker 通道 |
| submit / receipt / result | 现有 `delivery.rs::inject_submit_verified/observe_leader_receipt/CurrentTurnSubmitObserver`；`messaging/results.rs::report_result_for_owner_team_inner` | 保留当前兼容映射；新细分 evidence 不直接改旧 durable status |
| L1/L2 probe | 现有 `coordinator/steps/abnormal.rs::agent_process_liveness`；`os_probe.rs`；`Transport::has_pane/liveness/provider_exit_status`；`restart/rebuild.rs::restart_readiness` | 使用共享 host/OS 观察，不能每家复制 kill/probe；wrapper、native process 与 pane 分开 |
| L3 server/client/return probe | 现有 `mcp_server/wire.rs::McpServerLifecycleMarker`（尚无可用 epoch/ready consumer）；Pi runtime events | 新协议 observer 通过 §6 的 scoped `Probe<E>` 接 readiness；server write 不能代替 client received |
| semantic activity/fault | 现有 `coordinator/tick.rs::jsonl_activity_for_agent`；`provider/classify.rs::latest_explicit_error_fact`；`messaging/activity.rs` | H5 仅解释 turn/fault，不制造业务回复；无 reader 则 Unknown/Unsupported，不是空闲 |
| 会话归属/恢复 | 现有 `ProviderAdapter::capture_session_candidates/recover_session_id`；`provider/session_scan/`、`provider/session/` | 返回候选与置信依据，由框架按席分配；exact backing 不降为 sid-only |
| 公开 fork | 现有 [`lifecycle/launch/fork_agent.rs`](../../crates/team-agent/src/lifecycle/launch/fork_agent.rs) `fork_agent_with_transport/in_window_fork`；[`lifecycle/restart/avatar.rs`](../../crates/team-agent/src/lifecycle/restart/avatar.rs) `fork_pi_new_seat_locked` | **拟议**在此引入模式化准入/结果；旧 `ProviderAdapter::fork_plan/materialize_fork_backing` 仅 native plan 层 |
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

role effort 优先 TEAM；Pi 不继承 TEAM；两者没有就 native default。parse trim 但大小写敏感。`ultra` 含 Codex client delegation，不准降成 max。支持 flag 不证明每模型支持该档。新 Provider 对六档逐项作准入决策；新接入不复制 Gemini 的历史忽略习惯作为缺省。

Codex carrier 为 `-c model_reasoning_effort=LEVEL` 且在 profile config 后，Claude/Grok 为 `--effort`，Pi 为 `--thinking`；Cursor/Gemini 无该 flag。既有 inner 错误保字节：`cursor_agent does not support effort; the Cursor CLI has no --effort flag`、`effort 'ultra' is only supported by codex`、`effort 'max' is only supported by claude/claude_code/codex/pi`。各 CLI/compiler/spec/runtime wrapper、聚合顺序与 raw/trim 仍各有合同，不能只比较 inner 就声称公开错误等价。

Pi 特例保留：compile shape 校验不是 launch exact membership；首次 slash 两边非空不意味着只能一个 slash；内部空格可属于 exact ID；final raw 与 trim 不等要拒绝。非 Pi `validate_model=Ok(true)` 不是强验证；`verify_started_pi_model` 有定义/测试不等生产已用。

### 4.2 纯描述与物化

**规范 C04 — 命令不是 shell。** provider-local builder 只拼 argv；配置 JSON、Codex instructions escape、POSIX shellquote、YAML quoting 各用对应函数，不能共享一个“万能 escape”。`Some("")` 是否传给 native 由已声明入口决定，不能在 shared append 偷修。

目标设计中，executable/catalog 的有界观察、资源身份分配在纯 plan 前完成；写配置/role/extension 在 plan 后的 materialization 阶段完成。H2 使用已准入值、预分配 ID 与确定的 seat paths。当前 `build_command_plan` 有 UUID 分配、Pi materializer 交织这些步骤，不能因此宣称整个 trait 已纯化；迁移不为纯度直接重写现有调用。

```rust
// Target only. Specified means planned, NOT that native read it.
struct LaunchPlan {
    argv: Vec<String>,
    expected_session: ExpectedSession, // Preassigned(id) | CaptureAfterLaunch | NotApplicable
    materialization: Vec<OwnedResourceRequest>,
    carriers: CarrierReport,
}
struct CarrierReport { prompt: CarrierUse, mcp: CarrierUse, bypass: CarrierUse, effort: CarrierUse }
enum CarrierUse {
    Specified(CarrierRef), NotRequested,
    NotCarried { reason: Reason }, Rejected { reason: Reason },
}
```

规划时 `Specified`、H3 的 `Materialized`、native 的 `Observed` 是三份不同证据。新 Provider 收到必须的 Team prompt/MCP 却 NotCarried 时，应在启动前拒绝 Round-trip worker 模式；显式授权的 launch-only 模式可以降级并公开原因，不能 warning 后仍称完整 Agent 接入。空 `MaterializationReceipt` 不能代替 NotRequired；失败的 partial receipt 绑定 operation/owner/path/先前归属/写入摘要及恢复条件，凭据与原文件正文不得公开进日志。

- MCP server 必须指向当前 candidate 的绝对 executable，使用既有 `mcp-server --workspace ...` 与 captured Team identity。namespace 由 native client/runtime binding 决定，不猜固定前缀。
- 声明载体为 `ArgvConfig / ScopedFile / RuntimeBridge / Unsupported`，另记 scope（invocation / seat / cwd / global）。`native_mcp_config=false` 不等于没有 MCP。
- scoped 文件有 owner、权限、原内容/写入摘要与恢复条件。需要 global settings 的新接入必须单独获授权并有 compare-owned-content 恢复；不得照搬 Gemini installer 注释承诺不存在的 backup/restore。
- subscription、official_api、compatible_api 分别准入。没有 credential mapper 的模式拒绝/标未支持，不能靠 metadata 接受来宣称可用。日志只记录变量名与去敏原因，不记录凭据。

### 4.3 Env 与宿主

| Provider | bypass=true / false 的基线 argv | auth/profile 边界 |
|---|---|---|
| Codex | `--dangerously-bypass-approvals-and-sandbox` / 不加 | official/custom env-key mapper；compatible最终unset仍胜出 |
| Claude | `--dangerously-skip-permissions` / 显式`--permission-mode default` | official/compatible凭据通道与isolated config root不同 |
| Pi | 两者均无额外flag | role仅subscription；不能据此推native审批默认 |
| Cursor | `--trust --sandbox disabled --force` / 不加 | 无Provider credential mapper；proxy路径不是BYOK完成 |
| Grok | `--always-approve` / 不加 | login/folder-trust另门；catalog可见不是登录证明 |
| Gemini | builder不消费bypass | shared层可export GEMINI_API_KEY，但compatible最终unset；不证明endpoint/credential生效 |

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

| Evidence | 本次可证事实 | 不能升级成 |
|---|---|---|
| E0 Accepted | 真实 row/id 已持久化 | 已 paste |
| E1 Buffered | buffer 已装载 | composer 已收到 |
| E2 Pasted / Visible | paste effect、token/fold 可见，分别记源 | 当前 composer 正确、已提交 |
| E3 KeySent | 指定物理键调用成功；歧义错误另记 | native 已消费 |
| E4 ConsumptionEvidence | 本次输入离开 composer 或 native acceptance；弱 busy 只能标 hint | authoritative turn / 自然回复 |
| E5 TurnObserved | native turn 或明确标弱的 heuristic | Agent 已理解、执行完成 |
| E6 Receipt | 对应 transcript token / app-server turn acceptance | MCP 自然回包 |
| E7 NaturalReturn | Agent 在 captured identity 下发出的 correlated MCP 调用/结果 | 工具响应已被 client 读取、业务成功 |
| E8 BusinessChecked | 读取原始业务结果并核任务断言 | 其他模型/版本/平台全通过 |

E0–E8 是命题索引，**不是必须逐级升阶的单值状态**：RPC 可以没有 E1–E3，mailbox 不需要 E2；server 收到 E7 调用不证明 client 收到工具响应。旧 `delivered` 与 `SubmitVerification` 的兼容映射保持现状，新增 evidence 不把所有 worker 强改成只在 E4 才落旧状态。

### 5.2 Envelope、分隔符与发送函数

共同 `render_message` 的兼容字节形状：

```text
Team Agent message from <sender>[ for <task_id>]:

<content>

[team-agent-token:<message_id>]
```

最后 `]` 后**没有额外 LF**。content 原文保留 LF/CR/引号；正文换行不是提交键。末尾 token 是 Team correlation marker，不是 secret、native session id、模型 stop sequence 或回复结束符。

**规范 C07 — token 身份显式传递。** 拟议发送请求携带可信 `message_id` 和 renderer 产生的 marker/range；不能从任意正文“取第一个 token”作为权威。基线低层正这样取第一个，而高层 receipt 查实际 message id：迁移此差异必须单独有碰撞 fixture，不悄悄修改旧路径。

拟议接口只建立一条终端发送路径；**deliver_envelope 负责消息边界，Transport 负责唯一物理循环**，不是两套可任选的发送器：

```rust
// Target only: trusted token is not parsed back out of rendered text.
struct Envelope { rendered: String, token: Option<MessageToken> }
fn resolve_submit_policy(
    context: &DeliveryContext, profile: &InputProfile, interaction: &InteractionHook,
) -> Result<ResolvedSubmitPolicy, AdmissionError>;

// messaging::delivery: identity/intent/evidence orchestration; terminal channels only.
fn deliver_envelope(
    transport: &dyn Transport, target: &BoundTarget, envelope: &Envelope,
    policy: &ResolvedSubmitPolicy, observer: Option<&dyn DeliveryObserver>, deadline: Deadline,
) -> Result<DeliveryReport, NoEffectError>;

// Transport extension: ONE stateful physical executor; no Provider registry lookup.
fn inject_with_contract(
    &self, request: &InjectionRequest, policy: &ResolvedSubmitPolicy,
    observer: Option<&dyn DeliveryObserver>,
) -> Result<InjectionReceipt, NoEffectError>;

// Existing SubmitObserver has no step argument; this is a new target interface.
trait DeliveryObserver {
    fn observe(&self, event: &DeliveryEvent) -> Result<(), ObservationError>;
}
// DeliveryEvent distinguishes PhysicalStep(kind, ordinal, effect, binding)
// from NativeAcceptance(message, attempt, evidence).
```

- `DeliveryContext`：收件 Provider、native version、operation/channel、auth、backend。resolver 由 descriptor 数据与 H6 纯回调组装 immutable policy；没有 policy 不是 generic fallback，而是 Unsupported/Unverified。
- `Envelope.token` 在业务消息中必有且来自 durable row；ControlCommand 明确无 token，Wake 没有正文。`Text("")` 仍不是 Wake。native RPC 保留 shared renderer/token，由 messaging 分流直接走绑定协议，**不调用终端 seam，0 keys**。
- `InjectionRequest`：目标 endpoint/pane/generation、attempt id、可信 message id、payload kind、渲染 bytes、绝对 deadline。buffer 名同时绑定 scope/message/attempt，释放只释放本次自有 buffer，不能把共享名字当输入锁。
- `InputSample`：前后 capture 的范围/时序、可信本次身份、paste latch、步骤与 native receipt；H6 只做纯解析。`ResolvedSubmitPolicy` 可携带该窄纯回调；descriptor 本身不带行为，Transport 不接整个有 I/O 能力的 adapter。
- `SurfaceObservation`：Shell / StartupMenu / Composer / Confirmation / NativeQueue / Executing / Unknown；另含身份匹配三态、UI 区域/版本依据、latch 更新与 acceptance evidence。执行器在本次 attempt 保存 latch，H6 不保全局可变状态。
- `InjectionReceipt/DeliveryReport`：effect、stage、每类真实计数、观测及其证据强弱、problem、retry advice。`submit_keys_sent`、`submit_keys_confirmed` 与 `may_have_sent` 分开；无法确定成功的键不可伪造精确完成数。
- `DeliveryObserver` 显式收到物理步骤类型/序号/实例与独立的 NativeAcceptance 事件。现有 `SubmitObserver::after_physical_submit()` 无步骤参数，不能直接承担双确认语义；旧 caller 保旧接口，新路径的兼容桥只在明确对应提交点更新旧 current-turn 信息。
- `NoEffectError` 只适用于证明尚无终端输入/协议提交 effect 的失败；已写 buffer/intent 等本地资源仍需其 receipt。paste/key 子进程返回错误但可能已经执行、observer/save 出错，均要返回带问题的 receipt，不能把裸 `TransportError` 向外转换成“可重新注入”。

`deliver_prepared_message → resolve_submit_policy → deliver_envelope → inject_with_contract` 必须有真实 caller 测试。旧 `inject` 仍属兼容路径；新增 backend 的默认实现不能 `Ok(empty)` 或悄悄回落它。H2/H6/descriptor 也不会为了此 seam 被再包装成新的通用 Provider 类。

### 5.3 可扩展槽位：数据先行，有条件才加键

`InputProfile` 是 descriptor 内可审查的数据；`ResolvedSubmitPolicy` 是 operation×channel 应用限制、绑定 H6 后的执行输入。两者不是开放给用户任意 shell/key 宏的 YAML。最少包含：

| 槽位 | 允许表达 | 强制约束 |
|---|---|---|
| `framing` | bracketed paste；`Trailer::None` 或已证 native literal suffix | Team envelope 仍以 token 结尾；物理 bytes 可为 envelope+suffix，分层记载；suffix 同一次 paste，不伪装 Team marker；不自动补 LF |
| `submit` | `SingleEnter`；`ConfirmEnter { additional_enters, confirmation_rule }`；`GuardedRetry { max_retries, retry_rule }` | 用逻辑 `Key::Enter`；确认与重试互斥建模；计数均有限，先准入再 paste |
| `paste_ready` | 本次 token / 新 fold identity / 原生 composer 证据 | 定义 NeverSeen、Seen、Gone；旧 fold 或全文搜索命中不够 |
| `prompt_gate` | 本次 input surface、trust/menu/busy 识别 | shell prompt、native input prompt、确认菜单分别分类；不能仅靠宽泛 `>` |
| `timing` | paste-to-submit floor、各阶段 poll 间隔/超时、最小键间隔 | 单调时钟；poll 算入 floor；budget 耗尽不制造 Ready；sleep 本身不是证据 |
| `queue` | Preserve；或 `MarkAction { rule_id, key, max_presses, interval, region }` 的有界 Flush | 默认 Preserve；需 H6 确认本次 native queue UI，而不只是 bounded-tail 字符串命中；所有动作纳入总预算 |
| `acceptance` | 仅 KeySent / compositor consumed / native transcript / protocol receipt 的要求 | 说明证据强度，不能通过改名字升级保证；与业务回程分开 |

单 Enter 示例：`SingleEnter`，额外键 0，queue Preserve，最大 submit Enter=1。首次 Enter 后即使 capture 失败，也只读回，不补键。

双 Enter 确认示例：`ConfirmEnter { additional_enters: 1, confirmation_rule: CurrentMessageConfirmation }`。第一个 Enter 只打开 native 确认；**仅当新出现的确认界面能关联当前 attempt/message，且未开始执行**，才允许第二个 Enter。固定 pause 后无条件 `Enter, Enter` 不合规。确认未出现/身份不明返回 Unknown，不把第二键改称 retry。多行才出现确认的 UI 可以单行 1 键、多行 2 键；每种分支须声明，不允许“从 generic 重试开始”把确认混进重试。

本版不开放无条件多键 `SubmitSequence` 作为新 Provider 默认。若未来 native 真有不可观测的固定多键协议，须另行提交官方 framing/原子性依据、active-turn 反例和版本边界；“试过一次一键没发出”不足以豁免当前 guard。

MarkAction 的 literal、大小写、native version 都要固定，但 bounded Tail40 本身不能排除正文/历史伪标记。H6 必须给当前 UI 区域、baseline 变化、当前 paste identity 与步骤关系；无法区分则 Unknown，0 额外键。

多级确认可扩 `additional_enters`，每一步都要对应 native 证据与上界；不是盲发 N 次。准入时核 `additional_enters > 0`、`1 + additional_enters <= max_total_enters`、正 poll 间隔和总预算；运行时全部阶段共用剩余预算。网络或读取 retry 不消耗键预算。ConfirmEnter 默认 `retry=Never`；不能在确认不明时跨分支执行 generic retry。

总上界按**实际动作模型**计算：`1 + C + R + W + ΣQ_i`，其中 C=额外确认键上限、R=额外重试单键上限、W=已明确许可的 wrap-gap 单键、Q_i=queue 动作键上限。每次 retry 只是一键，**不能再乘整个确认序列长度**；全局 `max_total_enters` 与 deadline 还要约束各槽位之和。基线 generic ceiling 为 `1+0+2+1+8=12`（仅所有条件均满足时）；新双确认且不重试/flush 则 `1+1=2`。

**规范 C08 — 一次 paste，有限且有原因的键。** 对一次 attempt：

```text
validate binding + capability + budget
  → serialize input for (endpoint, pane, generation)
  → capture baseline / input-surface gate
  → set/load owned buffer → paste once → release owned buffer
  → await current paste evidence AND floor
  → prepare host mode → initial Enter → record physical-step evidence
  → [current confirmation OR guarded retry, bounded; prepare before each Enter]
  → [explicit queue flush, independently authorized and bounded]
  → observe receipt / return unresolved evidence
```

新协议在输入序列化失败时应 defer，不能无锁继续；现有 200ms 软锁是告警后继续，不能拿它当强互斥。每次键前 fresh 核 target/generation、host mode 与当前 rule；Unknown 不是“无 modal”。用户手工输入/并发 native 改变导致身份失配时停止自动加键。不得在失败分支重新 paste 或自动 Escape/Ctrl-C 清空 composer；不手工发送孤立 CSI201~。这不是全终端用户输入的原子锁，歧义仍必须返回 Unknown。

初始 Enter、确认 Enter 与 native acceptance 是不同事件；observer 的 current-turn 归属更新必须知道触发的步骤。不得因为第一键只是打开确认就标业务 turn 已开始；可持久化 physical-attempt intent，直到 native acceptance/明确的提交步骤才更新相应事实。

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
| StartupMenu | 无正文 buffer；Codex update `[Down, Enter]`，trust `[Enter]`；Claude trust `[Enter]`；handled 不等已消失 |
| EmptyWake | `InjectPayload::Empty` 直接一键，无消费检查；不把 Text("") 替换成它 |
| FastModeToggle | Codex `/fast` 的 Char 序列 + Enter；仅 cold `spawn_agents` 在 runtime.fast 下调用，不是 message-body 逐字输入 |

每个新 channel 必须明确 payload kind、是否 bracketed、policy 来源、预算和证据要求。非 Enter 键在当前低层不走同样消费循环，不能继承它的 submit 保证；新 native 特殊提交键需独立契约/证据，不按字节伪装 Enter。

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

### 5.6 Prompt-wait 与自然回包：两种不同的等待

1. **输入前**：验证真实 native input surface；已识别的 trust/update/login/确认菜单阻塞业务正文，复用同一 message row 排队。capture 失败不是“没菜单”；StartupHook/H6 报 handled 只说明尝试动作，必须重观察消失。没有 reader 不声称 observed-ready。
2. **paste 后**：只等待本次 token/fold/composer 的可按证据与 floor；不是等待某个通用 `>` prompt。大文本折叠、token Gone、NeverSeen 与超时分别记录。
3. **Enter 后**：只验证对应步骤的确认/消费/acceptance；framework 标 busy 则 batch 保队列；native busy 的额外键只按当前 policy，不能因等得久就 flush。
4. **业务回程**：使用 durable `wait_for_result` / FIFO watcher / collect / 自然通知，不解析 pane 最后一段文字当回复。早 collect 空不证明失败；事先确定观察窗口，首错不自行重发。
5. **结果到显示**：`result_id` 已写但 `leader_notified=false`/notification queued，保持“结果成功、通知未确认”；presentation silent/mailbox 不等待 pane 显示。业务是否成功仍查原始 payload。

等待的一切样本与当前 instance/message/task 绑定；模型输出中的“thinking/done/confirmation”不能伪造 host/UI/MCP 协议信号。

## 6. 三层探针合同

三个层级是不同命题，不是同一个 `ready` bool 的三个别名。采用强类型 **`Probe<E>`**：E 必须是该层专用 evidence，禁止由字符串/布尔值或另一层的 Probe 自动转换。

```rust
// Target only. Observed means evidence exists; acceptance remains a separate decision.
struct Probe<E> {
    subject: InstanceKey, sampled_at: ObservationTime, source: SourceRef,
    outcome: ProbeOutcome<E>,
}
enum ProbeOutcome<E> {
    Observed(E), Pending { deadline: Deadline },
    Negative { evidence: ContraryEvidence },
    Unknown(Reason), Unsupported(Reason),
    TimedOut { last: ObservationSummary }, Error(ProbeError),
}
type ProcessProbe = Probe<ProcessEvidence>;
type HostProbe = Probe<HostEvidence>;
// Protocol facets are separate Probe<ServerHandshake>, Probe<ClientBinding>,
// and Probe<OwnedToolReturn>; not one integer "ready level".
```

Negative 要有相反命题的正面证据，如当前实例已退出、当前菜单阻塞；没有数据/读失败/超时不是 Negative。`Observed(Exited)` 与 `Observed(Alive)` 的命题也不同，不能泛化 Observed=true。

| 层级 | 输入、权威事实 | 可以证明 | 禁止替代证据 |
|---|---|---|---|
| L1 Process | 已记录 native PID/归属、fresh cohort、wrapper authentic exit receipt、有限 OS probe | 当前 Provider 实例活着 / 已退出；缺 identity 则 Unverifiable | pane_pid、inert shell 活着、进程名相似、历史退出文本 |
| L2 Host / Tmux | selected endpoint、returned pane、session/window、generation、owner、capture/query | `Addressable`、`Owned`、`InputSurfaceReady` 各自独立；coordinator 另列 | has-session 当 input-ready；查询失败当空；coordinator 活着当 MCP 可用 |
| L3 Protocol / MCP | server handshake、owned runtime binding、实际 tools、client return/correlation | 下表的准确协议事实 | 配置已写、prompt 列了工具、turn busy、模型目录存在 |

**采样可以并行，ready 裁决必须显式组合。** 真实本次 L3 return 即使 L1 OS probe Unknown，也应保存为有效 L3 证据，而不是丢弃/改成 Unknown；它不能反推 L1 的 PID/归属。反向也不允许由 L1/L2 自动 promote L3。若同一 current subject 有退出/解绑反证，组合 readiness 不得 Ready，旧正面记录保留为历史并标失效。

**规范 C10 — L3 分级且不自锁。**

| Facet | 精确成功命题 | 不能证明 |
|---|---|---|
| Configured（前置事实） | 预期 MCP carrier 已物化 | native 读取/连接 |
| T3a `ServerHandshake` | 绑定本 agent/owner/generation 的 Team MCP server 收到 initialize/tools-list 并完成规定响应写出 | native client 实际加载三工具、已收到响应 |
| T3b `ClientBinding` / Available | 当前 native runtime 中 owned direct tools 或 proxy 的真实绑定可调用 | 三工具都返回、业务 payload 正确 |
| T3c `OwnedToolReturn` | 特定 tool/call 在当前 epoch 的 client-side return 或等强关联回执 | 其他工具已返回、业务成功 |
| BusinessChecked（验收，非新 probe 层） | 当前任务的请求/自然回程/原始业务 payload 和验收断言一致 | 全场景通过 |

server 接受 `tools/call` 是 Invoked；响应写出是 Responded；client runtime 的非 error tool-result 或能证明 client 收到结果的关联回执才是 Returned。**“server 记录首个 owned 调用”不能单独升级 T3c round-trip**，但它可证明 E7 自然发件发生。每工具计集合，不把一工具返回当三工具都返回。

基线 `mcp_server/wire.rs::McpServerLifecycleMarker` 已写 `mcp.server_started`、`mcp.tools_list_response_written`；后者的 `spawn_epoch=null`、`spawn_epoch_source="unavailable"`，包含 agent/owner/workspace/PID/PPID。当前所核 readiness 链没有其 epoch-bound consumer。未来 T3a 可以复用这些点位，但必须先有受框架控制的 instance token/epoch 传递、对应 server 实例关联与 reader；tools-list marker 只证明该响应写出，宣称 initialize handshake 还需对应证据。不能只增加一列后就宣称所有 Provider 自动已获 T3a，更不能用 profile 自填 epoch 冒认身份。

Pi 的 `ToolsAvailable` 是 direct registry 事实；proxy 可用时 stage 可能仍叫 RegistrationSelected，应按实际 binding evidence 而非标签判读。`ToolsVerified` 是三个 owned 非 error tool-result 事件集合，不代表每个业务 payload 成功。运行时 shutdown/unregister 后旧集合不证明新实例可用。

不能要求 T3c/BusinessChecked 后才能投递第一条任务；lazy MCP 在首次工具使用前缺 T3a 是 Pending/Unknown，不是失败。doctor 不得为“探针”自动 `report_result` 终结真实任务或循环发消息；优先只读 `get_team_status`，发送/完成回程在授权隔离验收任务内完成。

| 消费者 | 目标准入/结论 | 不能冒称 |
|---|---|---|
| 新协议正常 terminal delivery | fresh current target/owner、native-instance 与 input-ready 正面证据；无 blocker；L3 可到 Available 或按已批准 lazy policy 等待 | “L1不为Negative、L2不为Blocked”即默认放行 Unknown |
| 明确授权 bootstrap 验收 | 仍须 target/instance/input surface 足够，不往未知 shell/menu 粘任务；有限一次投递可建立缺少的 L3 证据 | Unknown 被改名 Available，或可无限重发 |
| quick-start/restart 报告 | 按实际 L1、host/infra、L3 分列；兼容旧输出 | outer Ready 等于工具 ready |
| abnormal/调度 | 当前 cohort、fresh fault 或 semantic activity；不同消费者分别判 Unknown | 无 reader 即 Idle、无 fault 即可自动重启 |
| 验收/清场 | 对应有限业务断言/owned 残留复验 | 由进程活着推业务、由业务回包推整机 clean |

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

先分清**计划、可执行 fork 模式、非 fork 的上下文派生**，不能让 `NativeArgv` 一个名字同时表示三者：

| 类别 | 新对象 / 身份 | 成功证明 | 不能承诺 |
|---|---|---|---|
| `NativeForkPlan`（H2 中间产物） | argv/准备计划，尚未启动 | native 参数及必要 backing 物化正确 | 公开 fork 可达、新 Agent 已创建 |
| `InWindowBranch`（公开模式） | 同 agent、同 pane；native 内部分支 | 本次命令之后的新 native 分支证据；已知失败文本另识别 | 新 MCP 身份、可恢复的新 session id、可事务回滚 |
| `NewSeatFullSnapshot`（公开模式） | 不同 agent/native session、独立 pane/Team identity | 完整 backing 事务 + 注册/启动/commit 收据；readiness 另列 | 屏幕标记即 snapshot、backing Verified 即工具 ready |
| `NativeNewSeat`（未来可选公开模式） | native fork argv 后实际新席/新 session | 公开 gate 已许可，实际 capture 新 binding，source 不变与失败 effects 有证据 | 因 NativeForkPlan 已存在就自动准入 |
| `ContextClone`（独立操作） | 有限上下文/配置副本 | clone 范围与来源明确 | full-session fork 或等价历史 |

拟议 `ForkPolicy::admit(provider, auth, public_surface, source)` 返回 Supported(具体模式) / Unsupported / Unverified；后两者在按键/创建席位前拒绝。typed result 至少有 `mode, source_identity, target_identity, evidence_origin, backing_state, readiness, effects/preserved, compensation_status`。原窗 `target_agent_id=source_agent_id`、`new_session_id=None` 合法，不造 UUID；H7 只负责声明的 native staging，公共 lifecycle 负责锁/注册/spawn/补偿执行。

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
5. **F4 commit**：保存 target resume tuple、coordinator 与健康状态；backing Verified 不自动设置 L3 ready。
6. **F5 receipt**：事务/audit 收据成功后完成角色保留/交接；失败时报告实际已提交阶段，不能伪装完整成功。

**Compensation 是 F0–F5 任意失败后的独立有条件路径，不是成功流水线的第六步。** 仅处理本次 target；先证明无 writer，再回收已证 owned 新 pane；backing 仍等 staged bytes、row/spec 仍属于本操作才删除/回退。source 在读取中变化、未知 writer、新记录追加、目标被并发改变时保留资源和角色，报告 preserved/partial，不删除新上下文、不恢复整个旧 workspace snapshot。每个阶段注入故障都要证明 source、leader、sibling/coordinator 不受误伤。

InWindowBranch 没有等价回滚机制。新契约要分辨旧 screen mark 与本次新证据；键已发但验收/审计失败，应报告 Unknown/Partial branch，而不是自动再 paste slash。unsupported/unverified 分支必须在任何按键或新 pane 前拒绝。

## 8. 停止、移除与资源清场

**规范 C12 — owned 不等于 exclusive，失败不等于零 effects。** Provider 只声明自己创建的资源，真正 stop/reset/remove/shutdown 由共享 lifecycle 做归属和残留判断。

- stop 保会话 backing；reset 必须 discard-session 并检查旧实例残留；remove 只移除目标注册和可证 owned role/wrapper，不删除用户外部 role、session 历史或 HOME 设置。
- 破坏动作前 fresh 验证 endpoint + generation + owner；精确 pane/PID。基线 per-PID 路径还要求所选 scoped roots 内的 OS argv exact pair `--workspace <canonical workspace>`，加 protected caller/ancestor 等门；不是全局搜索该字符串就获授权。进程名、裸 cwd、父进程继承环境不是单独的销毁授权。
- TERM/等待/再探测/必要时 KILL 仍须当前 identity 匹配、范围正确、audit 成功。查询失败不升级暴力清理；禁止 `pkill/killall/kill-server` 或广域 socket 删除。
- 基线 scoped shutdown 先做已证 per-PID effects，再过 session ownership gate。后者 Unknown 拒绝，不代表前面无 effects；不要把整个过程包装成原子失败。
- leader authentic receipt、caller/ancestor/PGID、live sibling、共享 coordinator 必须保护。即使 owned 空 endpoint 也不删 server/socket；不能为“残留为零”越权。
- 原生 Provider global config、用户 transcript、Pi fork 已追加记录的 backing 不属于常规清场可任意删除的资源。

| Descriptor 资源类别 | 默认处置 | 例子/边界 |
|---|---|---|
| OwnedRemovable | remove 时按 exact receipt + fresh owner 清除 | 本席 wrapper/临时 prompt 文件；stop 不等 remove |
| OwnedPreserved | 保留用户上下文 | session/backing、已追加的 fork records |
| Shared | 单席永不整体删除 | shared coordinator/规则/overlay 中他者键 |
| Forbidden | 无额外授权不写/不删 | 用户 global HOME config、他队 endpoint/资源 |

若新载体必须 GlobalInstaller，先获得单独授权；写前有私有受限 backup 与 compare-owned-content restore receipt，失败后只恢复仍属于本操作的 bytes。它不能把已有 Gemini installer 注释当恢复实现，更不让常规 seat teardown 获得 HOME 权限。

清场收据至少有：`scope, effects_attempted, effects_observed, spared, preserved, residuals, verification_errors, outcome`。outcome 区分 clean/partial/dirty/timeout/unknown；fresh probe 失败不能发 clean。停止命令返回、行政状态 stopped、physical cleanup 完成是三个事实。

## 9. 手把手接入第七家：Nova CLI（虚构示例）

**Nova 及以下 native flags/协议全部是假设，不是现有产品，也不在当前 Team Agent 可用。** 实际接入先用合法官方帮助/文档和有限实机确认替换假设；不通过猜 flag 完成开发。

### 第 0 步：写一张可证的 native 合同卡

先固定 CLI/version、official help/目录原始样本、短/长/Unicode paste、busy/第二键效果、startup modal、真实 tool binding、session/root 与退出行为；样本去敏。示例冻结的假设如下（刻意不复制任何现有 Provider 默认值）：

| 项 | Nova 的明确选择 |
|---|---|
| 身份/范围 | wire `nova_cli`，binary `nova`；第一版仅 subscription worker + Tmux；无 native leader verb |
| catalog/model | `nova models --json` 返回完整 JSON；ID 原文；无 model 用 native default，有值必须 exact catalog member |
| effort/permission | low/medium/high 原档 `--reasoning`；xhigh/max/ultra 明确拒绝；仅显式 bypass=true 加 `--yes` |
| prompt/MCP | `--system-file FILE`，`--mcp-config FILE`；seat-scoped 文件；官方可观察当前三工具 binding |
| 会话 | 假设官方 `--session-dir ROOT`、`--session-id SID`、`--resume SID`；archive header 含 sid/cwd；不支持 fork |
| 收件 | bracketed paste；无 suffix；短输入第一键直接接受，大 paste 显示带本次 marker 的 `⏎ again to send`，第二键才接受；H6 能可靠区分 |
| 观测/启动 | 当前确认可与正文/旧画面区分；accepted transcript 关联 message；首次 `Trust this folder? › Yes` 仅在该 scope 已获信任许可时可按；无 semantic turn reader |
| 预算 | 示例 floor=250ms、paste-ready=2s、confirmation=2s、acceptance=5s、poll=50ms、total=10s；均待 native 证据验证 |

任何假设无法证明就降级该能力/停止该条路径；不能把虚构预算写成经过调优的通用值。首次不支持 fork 是合格边界，不是需要假实现的缺口。

### 第 1 步：注册与准入，不先启动

1. `model/enums.rs` 增 `NovaCli`，`provider/wire.rs` 增 canonical/binary/显式 alias/ALL_PROVIDERS；目标 registration 绑定 descriptor+hooks。编译只能找穷尽 match 缺口，还要定向审查 `build_command_plan`、startup outcome、`enable_fast_mode`、paste floor、resume backing 的 wildcard fallback；未知值不得借此变 generic/Codex。
2. compiler、direct spec、role creation/runtime patch 的 provider/auth/effort 准入一致表达 Nova 合同；保现有入口的错误层次、raw bytes 与默认值。审查 schema/公开帮助，但不擅自增加 `team-agent nova` 命令。
3. 更新 catalog source/parser/action，保有界 runner 和 shared matcher；原始 fixtures 覆盖完整格式、重复、顺序、no-match、坏输出、超限、timeout。
4. 模型解析与 effort resolver 分开；同一请求从 role、direct spec、add/start/restart 均不能绕过准入。

### 第 2 步：填满 descriptor 与七个 hook bindings

| Descriptor 字段 | Nova 的显式值 |
|---|---|
| identity | `nova_cli` / `nova`；leader surface Unsupported |
| model | NativeDefault 或 raw byte-exact catalog member；不自动 trim |
| effort | `[Pass, Pass, Pass, Reject(xhigh), Reject(max), Reject(ultra)]`；`--reasoning`；TEAM 继承明确 true |
| auth | subscription Supported；official/compatible Unsupported（没有 mapper） |
| bypass | true→`--yes`，false→无 flag；拒凭父 argv 推断 |
| prompt | ScopedFileFlag(`--system-file`)，必要 H3 |
| mcp | ScopedFileFlag(`--mcp-config`)，invocation/seat scope，无 global install |
| tool_names | RuntimeBinding；不得自造 namespace |
| session | PreassignedFlag(`--session-id`)、SessionIdFlag(`--resume`)，exact owned root/backing 证据 |
| fork | 公开所有 auth 模式均 Unsupported(NativeNotProvided) |
| input | 单初始键 + 当前确认最多再一键；无 retry/queue；见下述 policy |
| startup | H6 识别上述 trust shape；授权 scope 内动作 `[Enter]`，之后重观察 |
| probe_sources | shared L1/L2 + H6；epoch-bound server 与 runtime client/return；无 semantic source |
| workspace | 每席独立 session/config root；不变 HOME；真实 working directory 明确 |
| teardown | own prompt/config 可移除；session 保留；他席/HOME 禁止 |

| Hook | Nova binding |
|---|---|
| H1 Catalog | Bound：有界 `nova models --json` 与完整 parser |
| H2 Plan | Bound：唯一 fresh/resume argv builder |
| H3 Materialize | Bound：0600 prompt/MCP文件、0700私有root + 完整/partial receipt |
| H4 Session | Bound：候选归属、exact backing probe；给 H2 ResumeBinding |
| H5 Semantic | Unsupported：activity 不伪装 Idle |
| H6 Interaction | Bound：startup/composer/confirmation/queue-absent 的纯解析 |
| H7 Fork | NotRequired：公开 fork 已明确拒绝，不是伪成功空 stage |

### 第 3 步：纯 argv + 有副作用物化

- 新建 `provider/adapters/nova.rs`：descriptor 字面数据、H2 native argv、H6 纯识别函数可在同一模块，不为七 hooks 强造七文件；在现有 facade/目标 registry 接线，复用 `append_pair/append_opt_pair`。
- 框架先分配 sid/seat paths；H2 argv 按 binary→bypass→model→reasoning→system-file→mcp-config→session 的固定顺序；不在 hook 内随机/读文件。`CommandPlan` 兼容投影保 expected sid/root；CarrierReport 的 Specified 不等 native 已读取。
- 必要的新 `lifecycle/launch/nova_mcp.rs` 仅负责 scoped prompt/config/root 物化与 runtime binding 观察（**拟议文件**）；复用公共 MCP serializer，partial failure 返回已写 receipt，不包含第二套 spawn。
- `spawn_agents`、`spawn_agent_window` 接相同 materializer；fresh/resume/失败补偿均记录自有资源。没声明 leader 支持就不要改 `leader/start.rs` 公开面。
- fake native/shim 检查 exact ordered argv、bypass true/false、unset 最终胜出、prompt 引号/LF、MCP file 的实际读取、ambient leader identity 被剥离。

### 第 4 步：把单提交/双确认接到真正消息链

Nova 的 provider-local policy（伪配置，不是当前 schema 字段）：

```text
channel           = WorkerTmux
framing           = BracketedPaste(suffix = None)
submit            = ConfirmEnter(additional_enters = 1,
                                 rule = CurrentMessageConfirmation)
stop_on_native_acceptance = true  # 短输入可在第一键后直接停止
retry_enters      = 0
queue             = Preserve
max_total_enters  = 2
acceptance        = NativeTranscriptForCurrentMessage
```

在 `deliver_prepared_message` 调 resolver→`deliver_envelope`→唯一 Transport executor，把可信 message id/attempt/binding 显式传下去。`additional_enters` 是有条件上限；第一键后若 native 已接受，立即终止后续键；若当前确认出现，最多再一键；两种都未观察到则 Unknown，不能“保险再按一下”。observer 只记录对应步骤，不伪造业务 turn/工具回程。未知 channel 返回 Unsupported，Wake 独立声明，不把确认套到 slash/trust/leader。

测试 oracle：一次 paste；短输入有权威 acceptance 时总共 1 Enter；大 paste 的当前确认出现后总共 2；无 native acceptance 且确认缺失、历史确认、wrong marker、busy、capture failure、deadline 到期均不产生第二键；后续 observer failure 不重新 paste。

### 第 5 步：三层探针与会话恢复

接 shared L1/L2，不新增 provider 进程扫描器；H6 trust action 必须复验菜单消失，未授权/未知菜单不粘任务。Nova binding observer 为 L3 提供来源/generation；通过授权任务分别看到三工具实际返回，不能拿 server `tools/list` 写完代替。

在拟议 `provider/session_scan/nova.rs` 接 H4/capture/archive parser；在 `resume_backing_probe_for_agent` 与 `requires_resume_backing` 显式接线。隔离同 cwd 双席验证不串 session；resume 后检查实际 sid/cwd/backing，不仅检查 argv。

fork 第一版在公共入口明确 Unsupported，断言 0 keys、0 new pane、source 未变。将来只有 native/完整 snapshot 证据到位，才另行声明模式并运行 §7 的故障矩阵。

### 第 6 步：编译、自动化与有限真实验收

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

`true` 仅用于已明确授权 bypass 的隔离环境；普通部署按用户权限选择，不默认扩权。通过公开 [操作指南](team-agent-operator.md) 建立完整隔离 TEAM/role 配置，固定 binary 绝对路径、source/hash、native CLI 版本、model、backend/endpoint 和 team scope。下列是有限用例设计，不授权在正在运行的用户 Team 上直接执行：

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
| T17 | descriptor 漏字段、wildcard generic、Required hook 缺绑定 | 显式注册/准入失败；仅编译通过不能替 caller 覆盖；无静态虚构 maturity |
| T18 | H3 写第二文件失败、H7 部分 stage 后失败、原文件被他者改动 | partial receipt 不丢；compare-owned-content 才回退；无 whole-workspace restore |
| T19 | L1 Unknown 但实际本次 L3 return；旧 epoch/外部 server；只 server wrote response | 保存真实分层证据但不反推 L1；旧/伪 epoch 拒；Invoked/Responded 不升级 Returned |
| T20 | H2 内随机/clock/FS、SessionHook 另拼 resume argv、profile 后改 model | 相同 ResolvedLaunch 输出相同；唯一 H2 assembler；effective model 无准入旁路 |
| T21 | 确认序列重放、generic retry 吞确认、slot budget 分别超限 | `1+C+R+W+ΣQ` 与实际计数一致；每次 retry 一键；任何 unknown 不切换到另一加键策略 |
| T22 | CarrierReport planned=used、launch-only 当 round-trip、lazy-MCP gate 自锁 | Specified/Materialized/NativeObserved 分开；必要输入未携带则拒绝完整模式；有限 bootstrap 不伪 Ready |

可复用导航：`tests/provider_effort_mapping_contract.rs`、`tests/model_fuzzy_lookup_red.rs`、`tests/provider_submit_verification_red.rs`、`tests/inject_exits_copy_mode_before_submit.rs`、`tests/fork_agent_five_shapes_red.rs`、`tests/mcp_3_tools_red.rs`（均相对 `crates/team-agent/`）。新增原生协议 fixtures 应进入对应 Provider 测试模块，不读取真实 credential/profile/session 内容来制造公开 fixture。

### 必选清单

- [ ] descriptor 所有 facet 有显式决定、registry 无 generic fallback；七 hook bindings 与条件能力一致。
- [ ] 能力卡逐项写 Declared/Reachable/Observed/Verified、auth/native-version/platform/channel 与未知项；成熟度只写进有收据的验收记录。
- [ ] 所有新入口在副作用前准入；provider alias 与 executable/model alias 分离。
- [ ] model discovery、exact/opaque selection、effort 六档、权限和 profile 分别有依据。
- [ ] H2 没有隐藏随机/I/O；H4 只产 binding；H3/H7 失败仍返回 partial receipt；argv 与物化分离。
- [ ] 共享 env/unset、identity、host wrapper/route 没被绕过；planned carriers 不冒称 native 已使用。
- [ ] MCP carrier 被 native 实际读取；工具名来自真实 binding；三 logical tools 不另造协议。
- [ ] 物理 policy 在生产 caller 使用；一次 paste；1/2/多级 Enter 有明确 guard、预算和真实计数。
- [ ] token/framing/suffix 与 submit key、native receipt、business end 分开；碰撞 fixture 覆盖。
- [ ] 读失败、busy、旧 UI、提交后 observer 失败不会触发盲重发；Partial/Unknown 可对账。
- [ ] `Probe<E>` 各层证据类型独立；epoch/lazy/timeout 正确保留；server Invoked/Responded 与 client Returned 分开。
- [ ] 业务探针不自动终结真实任务；真实高层观察不因低层 unknown 被抹掉，组合 readiness 仍按操作 gate。
- [ ] resume 强归属；fork 模式、auth 门和 unsupported 拒绝都有反例测试。
- [ ] 有明确 resource scope/补偿；owned writer/并发改变时保留；清场有限且复验。
- [ ] 自动化实际执行；有限 native 自然回程完成；source/binary/native-version 与收据对应。
- [ ] 未执行、unknown、unsupported、partial 单列；文档/帮助/catalog 注册与实现一致。

## 11. 六家兼容迁移与实施顺序

本提案不批准一次性重构六家，也不因现状未知开启额外缺陷修复。批准实施后建议按风险推进：

1. **M0 characterization**：锁定 §5.4 的真实 caller argv/keys/counters/结果边界；包括 Pi 未接 helper、裸 leader fallback、Gemini 部分接入；不拿 helper 测试替整链。
2. **M1 descriptor mirror**：从现有语义建立 total data + explicit hook bindings，先只读对照旧输出。每个 facet 迁移时只有一个执行权威；禁止旧 match 与新表各自演进。
3. **M2 新 Provider opt-in**：依本合同接 descriptor/七 hooks/typed receipt，旧六家保兼容路径。静态描述符不制造实际 ready/maturity，不悄悄新增公共 schema。
4. **M3 一条 channel 一次迁移**：`resolve_submit_policy → deliver_envelope → inject_with_contract` 取代该 caller 的 TLS，先比对相同输入/键序/marker/timing/错误；fallback 先显式保留旧 generic，不默认改变。
5. **M4 物化入口复用**：cold/dynamic 共用 H3 之前，分别刻画 env/profile/materialization 顺序、Codex fast-mode 与资源补偿；不能用 parity 的名字删除现有差异。
6. **M5 分层观测接线**：epoch-bound MCP marker + reader、typed `Probe<E>`、client return 证据分别审查；新增观测不等于启用新 gate。旧字段/事件字节保持或单独迁移。
7. **行为变更单独裁决**：可信 marker、soft-lock defer、paste-ready fail-closed、recipient/UI 限定 queue、Pi 真单 Enter、fresh branch mark、probe 更强 gate 都不是纯重构。对照 §12 各 D 项逐项授权，不借此扫全部旧问题。
8. **证据跟 bytes/影响失效**：文档/test-only 不要求全订阅重测；改按键/身份/资源的路径需要对应有限实机。已有 Pi/Codex有限回程证据不能扩大为六家/fork/Windows 全通过。

**本设计阶段到此停步。** 后续 M0–M5 只是可审查的次序，不构成实施/合并授权。最终接入的完成标准不是“trait 编译了”或“有一个绿色 PR”，而是：**声明的公开操作可达，有限真实任务能回程，未知边界被保留，失败不重复执行或越权清场。**

## 12. 历史偏差账本：事实、目标与迁移条件

保留 Opus D-01–D-18 的身份，补充 D-19–D-22；这是固定黄金基线与目标的差异记录，**不是缺陷清单、修复授权或本轮发布门**。任何项都不能因规范存在而自动改变当前行为。

| ID | 基线事实 | 统合契约位置 / 单独迁移要求 |
|---|---|---|
| D-01 | ProviderCaps 只有 resume/fork/native_mcp_config/writes_global_settings 四 bool | §1 total facets+证据；false 不重新解释成无MCP；保旧 wire/状态 |
| D-02 | paste floor/Cursor guard 经 worker caller TLS；fallback 没设 | §5显式 policy；各 channel 真正 caller 对照，fallback 不自动借 worker profile |
| D-03 | `payload_token_marker` 取正文第一个 token；高层查真实 message id | typed Envelope/token；正文碰撞 T06/T07 后单独迁移 |
| D-04 | `InjectReport.attempts` 含 capture retries，flush 另计 | §5真实分项计数；不从旧 attempts 推历史 Enter 数 |
| D-05 | Pi/部分 Cursor 单Enter/禁flush helper 定义与测试存在，未生产使用 | 单独决定 Pi policy 并有限实机；不得宣布现状硬 max=1 |
| D-06 | 所有非Cursor普通收件 caller 扫 Grok `Enter:send now` | recipient+当前 UI scoped MarkAction；扫描无mark虽0键，也不能当已按Provider隔离 |
| D-07 | 非Pi `validate_model` 返回 Ok(true)，不构成强模型准入 | §4 model policy；沿真实 caller 证明 exact/opaque 决策，不用方法存在当gate |
| D-08 | Gemini minimal/fallback argv 不消费 prompt/MCP/bypass/effort | CarrierReport 显式 NotCarried；普通 spawn MCP 连通未证，launch-only 不冒充 round-trip |
| D-09 | Gemini global installer 直接写 settings，注释所称 backup/restore 未实现；非普通spawn接线 | §8先授权+可恢复receipt，不得原样接 normal spawn |
| D-10 | status_patterns 定义不等 tick 的实际 activity 路径 | §5.6/§6：JSONL-first/generic边界；无reader不得假Idle |
| D-11 | cold/dynamic 各有 Copilot/Cursor/Grok 物化 arms；Codex `/fast` 仅cold | H3复用前分别characterize；不为了统一偷偷改变 fast-mode/env |
| D-12 | Codex caps.fork=true而public拒绝；Pi caps.fork=false而public快照新席 | §7 public ForkPolicy与native plan分开；保持auth门与拒绝文本 |
| D-13 | tools-list marker 的 epoch=null/source=unavailable；当前readiness无其epoch-bound consumer | §6 T3a需身份传递/reader；server responded不证明client returned |
| D-14 | 声明 POST_SUBMIT_CONSUMPTION 常量3/60ms，真实相关loop是inline12×100ms | §5.4记录实际值；取值统一须先锁真实计时，不能照常量改行为 |
| D-15 | effort-ignore event Gemini provider 字段是 `geminicli` 而非 `gemini_cli` | wire统一不顺手改既有事件字节；独立迁移 |
| D-16 | escape argv构造后丢弃；prepare忽略close-bracketed参数；相关helper存在不等发送 | 保禁止Escape/C-c/孤立CSI201；删dead code也不冒称修复输入行为 |
| D-17 | primitive / typed-plan / public 的resume/fork auth门不同 | 每surface显式准入，不能复制某一家门覆盖其他入口 |
| D-18 | verify_started_pi_model 有定义/单测，无生产接线 | 不声称启动后模型identity已核；启用需另行端到端证据 |
| D-19 | 200ms pane-input 软锁超时告警后继续 | 新协议 defer；实际并发/用户输入仍可能歧义；迁移为行为变化 |
| D-20 | paste-ready timeout/部分capture失败可继续submit，mode清理失败也可能继续 | 新规则Unknown不盲加键；保持旧caller兼容直至独立切换 |
| D-21 | 原窗fork依赖tail中的screen mark，非完整backing事务 | 新模式要求本次新证据；旧mark/capture/audit后失败不自动重复slash |
| D-22 | quick-start PendingToolLoad；restart ready只检查session/pane/coordinator | `Probe<E>`保真实分层；不把历史Ready字段重命名就当协议可用 |

更新某项行为的实现 PR 必须同时说明其旧行为对照、真实 caller、允许的兼容改变及有限验收证据；未实施项保持原事实，不写“已修复”。
