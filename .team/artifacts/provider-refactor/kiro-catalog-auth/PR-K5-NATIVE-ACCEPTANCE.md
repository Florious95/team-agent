# PR-K5 实机验收准入规范与四步终验说明书

**状态：准入规范交付；K5 NOT-RUN；不可直接派测。** 依据 leader `msg_d63a82ab75d8` / `msg_a701be8cc5f8`。本文面向准备者；准入全满足后另生成不含根因、源码提示或修复历史的 runner 四步包。

## 1. 验收问题与当前事实

目标不是证明 Kiro 会算题，而是证明：公开 Team 入口在隔离 scope 内启动真实 Kiro → 公共队列/物理执行器送达任务 → Kiro 自然调用 MCP 回报 → Luna caller 自然收到结果 → 精确 shutdown 后无本用例活进程/绑定/临时配置残留。

| 对象 | 已有事实 / 当前准入状态 |
|---|---|
| 被验证的库源码 | `fcbbc752c594e0f8107f1c047145d03db5f75244`，tree `667db1bd937de84212816dc12e32deeea6b74c78`；159P/0F/0I，test/clippy/fmt 全 0；不是可执行候选 |
| 原生 engine | `/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat`，2.28.0，SHA256 `430aae1a4f5ae252e785114d45d61ec465796ad6ca96383fb7ca383dd96b1712`；不是 Team Agent candidate |
| 认证/catalog | 用户已有登录；原始 1719-byte JSON / exit 0；9 个模型，H1 已绑定；无需读取 whoami/credential store |
| 本轮模型例外 | 仅被测 Kiro 可用精确 ID `claude-sonnet-4.5`；caller 和 runner 仍用 Luna。最新明确指令只适用于本用例，不修改全局调度规则，不选 Auto/别名/其他 Claude |
| effort | 原始 catalog 未报告 per-model effort。此例不设置 role/team effort，不由 CLI flag 语法推导能力；如果公开配置强制 effort，则先解决契约/证据，不能硬填 high/max |
| 可执行 candidate / coordinator | **未产出**。当前新包只有 library，没有原生公共 supervisor/MCP 子进程入口；不能用旧六家二进制、Linux 产物、helper 或测试程序冒充 |
| Native adapter | H2 仍明确拒绝非 Fixture；H4/H5/H6/H7 与 terminal profile 未验证；不能删除拒绝条件就称完成 |
| 本用例 workspace / config / 四条命令 | **尚未创建/冻结**。不存在可供 runner 执行的配置；本文的逻辑步骤不是已实现的 CLI 语法 |

认证状态不能缓存为永久成功；若新鲜 discovery 返回 AuthRequired，停止，绝不发起 login、重试认证或读取凭据。现有单轮 5535 回答不证明以下任何生命周期/MCP门。

## 2. 公共入口接驳要求（开发/准备者，不派给盲测者）

复用现有库端口，不增加 adapter 私有队列或第八个 hook，不修改旧六家：

1. **持久 supervisor 与公开 operator 入口。** 新的隔离入口创建/打开 `ContractStore`，使用 `Lifecycle::startup`、`supervisor::tick`、`Lifecycle::teardown` 和 `PhysicalRuntime`；定义真实公开的启动、发送、结果呈现、scoped shutdown 语法及配置解析。持久循环与 operator 返回有明确边界，进程启动成功不代表 Team ready。
2. **MCP 子进程入口与真实归属。** 将候选实际 stdio 子命令/argv 交给 `McpStdio`，接 `mcp::serve`。由已验证 transport 注入 `CallContext`（scope/seat/instance/generation、binding、connection、task），不从工具参数或任意文件认领 sender。三工具保持 `send_message`、`report_result`、`get_team_status`；不把 lifecycle 新增为第四个 MCP 工具。
3. **真实 native admission。** R0 固化 H4 session capture、H6 composer/提交 profile、H3 配置消费及 MCP 命名/隔离证据后才开放对应 H2 Native 路径。现有 Fresh 要求 CaptureAfterLaunch，不改成 NotApplicable 来绕过 SID。H5 无证据时保留 Unknown，不能从 prompt 推导 idle；基础四步不要求假装已支持 H7。
4. **实时证据生产。** T1 为确切 PID/birth/image；T2 为同 scope/current profile 的 composer；T3a 为 server initialize/tools-list 写出；T3b 为真实 Kiro client 的绑定/发现。配置文件存在不等于 T3b。T3c 分别记录 invocation、response-written、client-consumption、caller-presentation，不能互相提升；若某项不可观察，如实 Unknown。
5. **首条任务与回程。** 首条任务只走共用 outbox/一次性 bootstrap/K2 executor。若 T3b pending，仅在现有契约允许的 first-business 豁免内行事；反证不得被 bootstrap 覆盖。未知提交效果隔离，不自动重贴。绑定本用例的 Luna caller 公共呈现路径，禁止复用其他 Team 的 endpoint/上下文凑回包。
6. **退出与精确清场。** shutdown 等待已捕获 native/MCP/本 scope supervisor 归属对象退出，持久化终态后才 receipt-bound 删除 owned 配置。未知 PID、活 writer、changed inode/hash、共享对象一律保留并报失败/未知。不能 killall/pkill、kill-server 广域扫除、logout、删 native DB 或接管旧 Team。

这些是待实现/接驳义务，不是当前已有命令清单。入口实现后必须先经相称自动化验证；native profile 只能来自真实证据，不能把 Fixture 改标签为 Native。

## 3. R0 物理边界采集表

由独立 Luna 准备上下文按批准的有限探查步骤采集，不是 K5 盲测；开发席不执行 native。每项绑定 engine hash/version、macOS/architecture、harness/UI、model、cwd、scope/instance、命令/动作、时钟、原始退出与观察窗口。只读公开 help 不代表相应交互已通过。原始记录留在隔离证据目录；不收集环境全量、whoami、token、认证库或账户 session 数据库。

| 项 | 必须获得的物证与判据 | 未成立时 |
|---|---|---|
| R0-1 identity/catalog | engine 当前身份、现有用户登录下有界 H1 discovery、完整 JSON + exit 0；精确 sonnet-4.5 存在。旧 9 项记录保留，不造 Luna | 阻止候选准入；不自动换安装/模型/认证 |
| R0-2 agent/config/MCP | 先取得原生 agent/MCP help 的真实语法，再核本 scope `.kiro/agents` 配置的 validator、加载优先级、prompt 消费、三工具实际命名、ambient 配置隔离与 bypass 是否另需确认 | 不将计划中的 JSON/`agent validate` 写成已消费；未授权全球配置不修改 |
| R0-3 输入表面 | 实际 v3/UI 提示符、启动/风险确认、composer/忙碌/阻塞/错误状态；保留带 ANSI 与尺寸的观察，建立有限 fixture | 不猜 `>`、spinner=idle 或直接放行 H6 |
| R0-4 物理提交 | 长/多行/CR/Unicode/marker payload 的 paste 模式、换行、trailer、Enter 次数、确认 guard、时序与截止；同一原生 lane 的 before/after 和效果收据 | 不设“两个 Enter/500ms”假默认；未知效果不补键/重贴 |
| R0-5 session | 仅由当前已绑定 pane/protocol 获得 SID 及 provenance；fresh 的确切捕获路径，不靠最近文件、列表位置或账户库 | Fresh 的 H4 门保持关闭；不宣称 resume |
| R0-6 MCP 回程 | 实际 native client 启动候选 stdio 子进程，正确 connection/context，handshake、tools-list、client binding、真实调用/响应及自然呈现分层收据 | 不让 server alive/accepted/durable result 冒充完整回程 |
| R0-7 退出/清场 | 自有 native 正常退出、失败/认证/timeout 的实际分类；本 scope quiescence、owned 配置删除与共享资源未动的 receipts | 退出 0 不当业务成功；无法核归属就保留 |
| R0-8 可选能力 | 若要宣称 restart/resume 或 InWindowBranch，另采 exact SID/turn/parent-child 证据并运行独立用例 | 不阻塞有限基础四步，但能力保持 Unverified/NOT-RUN；FullSnapshot/NativeNewSeat 继续 Unsupported |

R0 是有限资料准备，不是无限探查。首个意外错误即停该项、记录事实与未执行项；后续改步骤/候选由准备者形成新轮次，不向盲测 runner 现场追加手工补救。

## 4. 派测前一次性冻结包

准备 owner 将以下全部实值写入一份 candidate receipt；任一缺失则 `admission=BLOCKED`，不能把本表原样传给 runner：

- **固定候选：** source HEAD/tree、Mac candidate 与 coordinator 的绝对路径、SHA256、version、target/架构、构建工具链/命令/真实 exit；候选由授权 CI/构建者产出并核为匹配目标架构的真实 Mac 产物，禁止本机 Mac Cargo。Grok Linux 测试结果不是 Mac 二进制。不在测试中升级/替换文件。
- **隔离域：** 新建私有绝对 runtime root、空业务 workspace、独占 tmux endpoint、本用例 scope/seat/instance、被授权的 caller 呈现路由；显式声明所有 owned 对象。不能借用已有 Team 的 scope、DB 或 endpoint。
- **完整配置文件：** 绝对路径和 hash；`provider=kiro`、精确 `claude-sonnet-4.5`、已批准 NativeExistingSession、显式 bypass、无 role/team effort；实际 candidate MCP invocation、bounded profile、唯一 task/case ID 与所需角色必填字段。这里只规定内容，具体字段名必须来自已实现的公开 schema，不编造可执行 TOML/JSON。
- **准确操作：** 根据候选公开帮助生成四条真实命令/脚本，统一固定 `BIN`、`SCOPE`、配置与真实 exit 捕获。wrapper 不替换产品逻辑、不伪造回包、不隐藏重试、不由 tester 拼路径。
- **证据/清理：** 证据目录、每步 stdout/stderr/exit 与单调时间、owner 清单、scoped cleanup 入口。日志保留与活运行资源分别列出。
- **窗口：** 本基础用例启动最多 120s、任务自然回包最多 180s、shutdown 最多 60s；这是预先冻结的验收观察窗口，不是生产 Enter 时序。若 R0 证明需调整，派测前修改并冻结新包，执行中不延长凑绿。
- **前检：** 配置、路径、SHA/architecture、native identity、模型和待测能力一致；由准备者核验，不消耗盲测去发现不完整配置。未改产品的既有证据可按 bytes 桥接；新增入口/native profile 的新验证不可借用本次 159P。

## 5. 准入后交给 runner-luna 的四步终验

干净 runner 只接收上节已冻结的候选/目录/完整配置、下面四步、窗口、预期与停止条件；不接收本文件的源码定位/R0失败历史。只使用该包真实公开命令，不自行查库/查代码/换模型/改配置/手工按键。

| 步骤 | runner 的唯一公开操作 | 完成标准 |
|---|---|---|
| 1 启动 | 在指定隔离工作区运行包中固定的 start 命令 | 120s 内公开结果为本 scope 的真实 Kiro worker 启动可接收任务；accepted/queued、进程存活或 config 写入本身不等于 ready |
| 2 派题 | 用固定公开 send 向该 worker 发送一次：`验收编号 <冻结case-id>。请计算 137 × 29，仅通过已加载的 report_result 工具回报一次，summary 为 <case-id>:<数字>；不访问文件、shell 或网络工具。` | 记录真实 message/task ID；只发送一次。此时 accepted/durable 只是入队，不判回包成功 |
| 3 自然回程 | 不发新消息，使用包中公开等待/呈现入口，在完整 180s 窗口内等待 | Luna caller 自然收到本任务的 `<case-id>:3973`；由 Kiro 实际 report_result 回程关联到本实例/任务。不能用 pane 中数字、helper stdout、tester 代报或手工消息替代 |
| 4 精确 shutdown | 运行包中固定的 scoped shutdown 命令，保留证据 | 60s 内本用例 owned Kiro/MCP/私有 supervisor/私有 tmux 活对象及路由均退役，owned 临时 agent 配置按 receipt 清除；不影响其他 Team。以产品公开清理收据为准，不以命令 accepted 为准 |

`<冻结case-id>` 由准备者在生成最终任务文本时替换为单一真实值，runner 不需要填写变量。首个意外错误或步骤外补救需求即停；保留原始返回和现场，不能自行执行后续步骤“凑完整”。失败现场的取证与 scoped 清理由指定 owner 在确认后处理，不把活现场遗留掩盖成第四步 PASS。

## 6. 终验判定与收尾

- **基础 PASS** 必须四步都满足；独立分层列出入队、物理提交、native MCP invocation、result durable、response-written、client consumption、caller presentation，不把缺一层的观察补造出来。若某非判据层不可观察，明确 Unknown，不能宣传它已验证。
- **FAIL / 准备或工具故障 / NOT-RUN / UNKNOWN** 分开：完整窗口内有可靠收据却无预期回包为用例失败；窗口/退出收据丢失则未知；未准入不执行则 NOT-RUN。首错根因另查，不把它默认归咎模型或产品。
- **“零残留”仅指本用例运行/临时资源。** 不要求删已归档证据、owned-preserved session backing 或共享缓存；保留项逐条给路径、owner、原因。无法证明停止/归属的对象报告残留/未知，不强删。默认 native 账户、订阅、其他 Team、共享 coordinator 不在清理权限内。
- 四步 PASS 只能称 **Kiro 基础 startup/send/MCP/shutdown 接入通过**，不是 restart/resume/Fork 全功能通过；后者分别需要独立有限用例。unsupported 模式拒绝是正确契约，不伪造成 native 能力。
- 最终收据绑定 candidate/config/profile/source、步骤时间线与原始退出、真实 result/call/instance 身份及 cleanup 证明。新的候选必须完整重跑同一失败用例；不向在途 runner 追加开发提示。

**本次交付只冻结以上准入与判据，未生成可运行盲测包、未启动 K5。** 下一 owner 先实现第 2 节与取得必要 R0，再填写第 4 节并派发干净四步包。
