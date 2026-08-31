---
name: F3 Provider、模型、权限与资源组合
type: requirement
status_at: 0.5.57
architecture: C1 C5 C7 C8
---

# F3：Provider、模型、权限与资源组合

## 一、用户获得什么

用户可以把不同 CLI 外壳、订阅账号、官方或兼容 API 模型组合进同一个团队，让高能力模型承担澄清和审查，让低成本或专项模型承担执行，同时不泄露凭据、不让 worker 获得高于 leader 的权限。

用户可见承诺：

1. **Provider 与角色正交**：角色说明“做什么”，provider/model/profile 说明“由谁、用什么资源做”；更换资源不会悄悄改写角色本体。
2. **跨厂商可组合**：Claude、Codex 与兼容 API 模型可以分别承担 leader 或 worker；消息和任务语义不因 provider 不同而改变。
3. **资源选择透明**：leader 能看到可用 provider、模型、授权模式、会话和健康事实，再决定如何分工；基础设施不替用户偷偷切换模型。
4. **凭据不进入对话资产**：角色文档和团队文档只引用 profile；真实 key、token、base URL 密钥部分不进入可提交文档、日志或 agent 上下文。
5. **权限只降不升**：worker 的审批/沙箱能力不得高于 leader；恢复、重启、克隆和分叉后仍保持这一上限。
6. **失败边界清楚**：登录缺失、profile 不完整、模型不可用或 provider 不支持某项会话能力时，必须明确拒绝或标部分，不能静默换成另一个 provider 或 fresh session。
7. **grok 启动前身份槽对账（在途，锚 `10137cda`，未发版）**：grok 席启动前必须对账目录级 `.grok/config.toml` 的框架每席键并清掉再启动；清不掉则拒绝。清单外的键不许动。身份隔离的完整承诺与黑盒见 [[F7 会话身份、续作、克隆与分叉]]。
8. **grok 思维强度只认角色定义（在途，锚 `10137cda`，未发版）**：`effort` 的唯一入口是角色定义；启动 team / add / 分身 / 重启都以它为准。期间不把 effort 写入 `.grok/config.toml`。角色没写 `effort` 时框架一个 flag 都不发，不许设框架内建默认档。编排侧生成的 grok 角色文件写成 `effort: medium` 是角色文件显式值，不是框架默认。
9. **cursor 作为 CLI 外壳接入（在途，锚 `10137cda`，未发版）**：cursor 可作团队成员的 provider；消息和任务语义与其它 provider 相同。cursor 不把父进程 env 传给 MCP 子进程，身份必须写进该席 `<workspace>/.cursor/mcp.json` 的 `env` 表——grok 那种靠 pane env 继承、不写 json 的做法在 cursor 上走不通。每席一个工作目录、一份 MCP 配置。角色注入走 `<workspace>/.cursor/rules/*.mdc`。写完 mcp.json 必须使 MCP 获准加载，否则工具恒为未加载。未在官方 help 列出的 flag 不进主路径。不承诺工具白名单。订阅路径启动 cursor 时必须把代理键透传进 worker 环境（只承诺有无；值内含凭据，任何页面、日志、报告不得出现值）。
10. **Pi 可作为 leader 或 TeamMate（用户会签 2026-08-31）**：Pi 可以作为 Team Agent leader 或 TeamMate。已由其他 Provider 建立的团队可以添加 Pi TeamMate。同一团队可以运行多个 Pi 席位，席位身份不得串位。落位地图 [[Pi provider 适配]]；添加成员见 [[F2 角色、成员与能力编排]]；已有团队不重新认领见 [[F1 对话开团与团队生命周期]]。
11. **Pi 原生行为一致性（用户会签 2026-08-31）**：通过 Team Agent 启动 Pi，应尽量等同于用户直接运行 `pi`。两者共享 Pi 的原生配置、认证、插件和默认行为。Team Agent 只增加协作所必需的每席位 MCP 身份和会话隔离。隔离与会话边界见 [[F7 会话身份、续作、克隆与分叉]]。
12. **Pi model/effort 可省略且不合成（用户会签 2026-08-31）**：`team-agent pi` 在省略 model 和 effort 时可以启动，并使用 Pi 的原生默认值；不得因此合成 model/effort 参数。Pi TeamMate 角色同样允许省略 model 和 effort。用户显式提供的合法 model/effort 应原样传递；非法值按 Pi 能力明确拒绝，不得静默改写。
13. **Pi 沿用共享角色字段（用户会签 2026-08-31）**：Pi 沿用 Team Agent 已有的共享角色字段及权限语义，包括 MCP、tools 和 `dangerously_skip_permissions`。不为 Pi 发明第二套字段、别名或 `auto`/`skip` 等不同表达。不建立新的通用 Provider schema。
14. **Pi 与 adapter 版本/digest 仅诊断（用户会签 2026-08-31）**：Pi 与 adapter 的版本号、包版本和 digest 可以作为诊断信息。当所需协议与能力满足时，不得以精确版本或 digest 作为 Provider 准入门槛。本条不影响发布物自身的版本、SHA 和供应链完整性核验，见 [[F10 治理边界与黑盒验收]]。

明确非目标：

- 不为所有 provider 排列组合分别实现一套消息或交付语义。
- 不在后台调用 provider API 做健康探针或隐藏消费 token。
- 不把 API key 写入 role doc、TEAM.md、事件日志或模型上下文。
- 不让 provider adapter 反向决定角色、任务编排或用户的成本取舍。
- 不把未验证的 cursor 工具限制 flag 写成“支持工具白名单”。
- 不把 grok 角色文件里显式写的 `effort: medium` 说成框架内建默认档。
- 强制用户填写 model 或 effort。
- 从团队级默认值重新合成 Pi model/effort 参数。
- 用精确 Pi/adapter 版本或 digest 建立准入硬门。
- 为 TeamMate Pi 建立独立的原生配置、认证或插件环境。
- 添加无需求依据的 `--no-*` 禁用参数。
- 为 Pi 发明第二套 MCP、tools、permission 字段或 `auto`/`skip` 等不同表达。

准则锚：

- `docs/team-agent-foundation-and-boundaries.md` §8「能力扩展的双轴模型」
- 同文 §10「不替代模型决策」
- 同文 §11「Provider、Model 和资源编排」
- 同文 §13「控制平面与模型上下文」
- 同文 §21「非目标」

## 二、怎样黑盒判真

### 跨厂商角色组合

原文锚：

- `docs/team-agent-foundation-and-boundaries.md` §11：必须支持 Claude 订阅 leader + Codex 订阅 worker、Codex 订阅 leader + Claude 订阅 worker，以及订阅模型与第三方 compatible API 混合
- `docs/项目设计与定位备忘录.md` 三.1「Claude 当 leader，跨厂商混合编码」
- 同文三.2「模型预算约束下的工种分工」
- `docs/team-agent-codex-foundation-rust-goal.md` §5 In Scope 第 4 条：subscription/API-compatible profile 抽象和 secret-safe 模板

黑盒验收：

1. 用两种不同 provider 建立 leader/worker 或 worker/worker 组合。
2. 双方完成一次消息、任务与结果往返；业务信封语义和同 provider 组合一致。
3. 状态中每个成员显示其真实 provider/model/profile，不发生角色或会话串位。
4. 切换成员使用的资源必须由 leader 明确选择，不由运行时在失败后自动替换。

### Profile 与 secret 安全

原文锚：

- `docs/team-agent-foundation-and-boundaries.md` §11：角色文档和 manifest 不得含明文 secret；runtime 只能报告 secret 是否存在、是否可用
- `docs/team-agent-codex-foundation-rust-goal.md` §6.2 `secret_redaction` / `profile_model`
- 同文 Phase 4「Profile and Secret Safety」：生成空白模板、默认忽略真实 profile、日志脱敏、缺失时报清晰错误
- 同文 §8：自动测试必须覆盖 profile secret safety
- 同文 §12：profile secret safety 有测试

黑盒验收：

1. 创建 subscription profile 与 compatible API profile；生成物不预填真实 secret。
2. 用本地填写的 profile 启动成员，角色文档和团队文档仅出现引用名。
3. 对运行状态、诊断、事件与失败输出做全文检查，真实 key/token 不得出现。
4. profile 缺字段或凭据无效时，在创建成员前或启动早期明确失败，并给本地修复动作；不得要求用户把 key 粘贴进对话。

### 权限与能力上限

原文锚：

- `docs/team-agent-foundation-and-boundaries.md` §10：系统提供 provider 状态和工具事实，由 leader 判断
- `docs/项目设计与定位备忘录.md` 二.4、三.6：leader 通过“看 + 按”处理需要人工确认的场景，而不是写死业务规则
- `docs/team-agent-codex-foundation-rust-goal.md` §5：profile 抽象与 secret-safe 模板属于正式范围

黑盒验收：

1. restricted leader 启动 worker，worker 不得获得 bypass/dangerous 权限。
2. bypass leader 启动 worker，worker 至多镜像同级能力，不额外扩大权限。
3. 对 restart/reset/clone/fork 分别复核权限上限与 profile 归属，不能只有 fresh-start 路径正确。
4. 遇到 provider 原生 approval 时，系统呈现明确事实并等待 leader 决策，不隐藏批准、不伪造完成。

### Provider 会话能力诚实披露

原文锚：

- `docs/team-agent-restart-foundation-goal.md` §9.1、§9.2：Codex 与 Claude 都必须以停止前后上下文连续作为恢复证据
- 同文 §12 第 2、8、9 条：provider adapter 提供捕获/恢复能力，Codex 与 Claude round-trip 分别通过
- `docs/team-agent-foundation-and-boundaries.md` §15：runtime 提供 session 与恢复事实，恢复还是新开由 leader 决定

黑盒验收：

1. 每个宣称支持恢复的 provider 都做一次“记住唯一标记→停止→恢复→答出标记”的真实 round-trip。
2. 不支持恢复的 provider 明确显示“不支持”，不能返回成功后新开空白上下文。
3. session backing 尚未形成时显示 pending；找不到或归属冲突时拒绝，不能拿同目录最新会话冒充。

### grok 思维强度 effort（在途，锚 `10137cda`，未发版）

原文锚：用户裁定 2026-08-18。允许集与拒绝行为为派单给定实测（本页不另测）：允许 `xhigh | high | medium | low`；`max` 会被 CLI 拒绝启动，不是静默降级。

1. 角色写明 `effort: high`（或允许集内其它值）：经启动 team / add / 分身 / 重启四条入口启动后，该席按该档运行；`.grok/config.toml` 不因本次启动被写入 effort。
2. 角色不写 `effort`：框架不代填任何档；该席按用户自己的 grok 默认运行，不得变成框架内建档。
3. 角色写 `effort: max`：启动必须被拒绝，不得降成某一允许档后继续。
4. 不能只验 fresh start；add / 分身 / 重启缺一则本条未闭合。

### cursor 接入形状（在途，锚 `10137cda`，未发版）

原文锚：2026-08-18 派单给定实测（本页不另测）。与 grok 的差异本身是需求事实。

1. 启动 cursor 席后，该席身份出现在该席工作目录的 `.cursor/mcp.json` `env` 表中；不得只放在父进程环境里指望 MCP 子进程继承。
2. 两席各用自己的工作目录：各自 mcp.json / rules 互不覆盖。
3. 角色正文出现在该席 `.cursor/rules/` 下的规则文件中。
4. 只写 mcp.json、不使 MCP 获准加载：工具保持未加载，不得自称已接入。
5. 注入把文本与回车分开发、间隔不少于 1 秒，才能成为该席真实输入；合并发送导致第一次回车被吞掉则本条失败。
6. 主路径不使用 help 未列出的 flag。不得把工具限制 flag 写成已支持。
7. 订阅路径启动时 worker 环境有代理键（只核有无）；任何可观察输出都不含代理值。

### Pi provider 适配（用户会签 2026-08-31）

原文锚：`.team/artifacts/pi-provider-adaptation/requirements/pi-provider-requirement-final.md`。条款地图 [[Pi provider 适配]]。本页不另测、不补充定稿以外的产品结论。

1. Pi 可以作为 Team Agent leader 或 TeamMate。
2. 已由其他 Provider 建立的团队可以添加 Pi TeamMate。
3. 同一团队可以运行多个 Pi 席位，席位身份不得串位。
4. 通过 Team Agent 启动 Pi，应尽量等同于用户直接运行 `pi`；两者共享 Pi 的原生配置、认证、插件和默认行为；Team Agent 只增加协作所必需的每席位 MCP 身份和会话隔离。
5. `team-agent pi` 在省略 model 和 effort 时可以启动，并使用 Pi 的原生默认值；不得因此合成 model/effort 参数。
6. Pi TeamMate 角色同样允许省略 model 和 effort。
7. 用户显式提供的合法 model/effort 应原样传递；非法值按 Pi 能力明确拒绝，不得静默改写。
8. Pi 沿用已有共享角色字段及权限语义，包括 MCP、tools 和 `dangerously_skip_permissions`；不为 Pi 发明第二套字段、别名或 `auto`/`skip` 等不同表达。
9. Pi 与 adapter 的版本号、包版本和 digest 可以作为诊断信息；当所需协议与能力满足时，不得以精确版本或 digest 作为 Provider 准入门槛。
10. 本条不影响发布物自身的版本、SHA 和供应链完整性核验。

## 三、现在处于什么状态

**0.5.57 时点：部分。**

已实现：

- provider、model、auth mode 与 profile 已作为角色运行元数据分离。
- subscription 与 compatible API profile、secret-safe 引用和脱敏已形成公开能力。
- Claude、Codex、Copilot 等 provider 已有独立会话归属与恢复语义；0.5.57 发版门含三个真实订阅场景串行验收。
- fresh/restart/clone/fork 等成员路径受统一角色、provider 与权限上限约束。

尚不能判为完整实现：

- 准则 §11 承诺的是跨厂商组合能力，不只是各 provider 单独能启动；现有 0.5.57 发版记录只证明三个订阅场景通过，没有覆盖全部 leader/worker 方向与 compatible API 混合矩阵。
- 部分 provider 的 resume/turn-state 能力仍不对称；不支持项虽可诚实披露，但“所有组合都具备同等会话连续性”尚无证据。
- Windows-native 在历史记录中仍有真实 provider 登录/订阅验收缺口；不能用编译通过替代真实资源组合验收。

发布证据：

- `.team/TASKS.md` §4.11「0.5.57 发版闭环」：门⑤“订阅 3 串行”通过，tag `v0.5.57`、bump `1f95363`、npm latest=0.5.57。
- 同节 fork 与会话归属记录证明 provider backing 需要 typed pending/rejected，而不能用启动成功代替归属成功；0.5.58 修复不计入本页现状。

对应架构：

- [[角色与模型分配]]、[[模型会话归属]]（C1）
- [[模型提交行为]]（C5）
- [[模型会话扫描]]、[[安装与资源]]（C7）
- [[平台与隔离契约门]]、[[生命周期与状态契约门]]（C8）

## 输入从哪个功能来

- [[F2 角色、成员与能力编排]]提供角色、职责与所需能力。
- [[F1 对话开团与团队生命周期]]提供 team scope 与本轮启动/恢复动作。

## 输出到哪个功能去

- 向 F4 消息寻址、受理与可靠送达提供 provider-neutral 的成员端点。
- 向 F7 会话身份、续作、克隆与分叉提供 provider 会话能力事实。
- 向 F8 状态观察、诊断与人工干预提供 profile、权限和 provider 健康事实。
- 向 [[Pi provider 适配]]提供 Pi 专属条款的 Provider 落位。

## 需求已演化待换代

1. `docs/team-agent-codex-foundation-rust-goal.md` 与 `docs/team-agent-skill-hardening-goal.md` 把真实验收收窄为 Codex-to-Codex，并将 Claude/其他 provider 列为当轮 out of scope；这是阶段范围，不得覆盖基础边界 §11 的长期跨厂商承诺。
2. `docs/team-agent-restart-foundation-goal.md` 把 session 缺失时 fresh spawn + warning 作为默认；现行需求更强调显式 pending/rejected 与用户选择。此项与 F1-3 同源，列入 [[待用户会签清单]]。
3. `docs/项目设计与定位备忘录.md` 三.6 把权限确认描述成 leader 读 pane 后直接按键；当前多 transport/provider 形态要求“看 + 按”保持通用原语，不能把 tmux send-keys 固化成产品承诺。
4. 基础边界 §11 使用“必须支持以下组合”的绝对表述，但没有定义最低 provider 矩阵与每种组合的黑盒验收深度。需要用户会签：以能力分级诚实披露为完成，还是要求固定跨厂商矩阵全部真机通过。
