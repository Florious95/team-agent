# K4 Kiro adapter：受证据门禁的文档实现（部分交付）

**不是可运行 Kiro 接入，不是 R0/原生验收通过。** 本模块实现可独立检验的参数/配置构造与 owned materialization；原生门禁保持关闭。不能用修改 `EvidenceKind` 或复制 fixture descriptor 的方式派生可运行 profile。

## 输入事实，而不是期望能力

研究档案 `kiro-discovery/RESEARCH-RECEIPT.md` 记录：

- 2026-10-08 观察到 macOS app bundle `CFBundleShortVersionString=2.28.0`、binary SHA256 `dee3f382fc8f6734fe505b815d786ed634ba67a258ba34986eca50f4f0b5fc22`。
- `--version`、`--help`、`chat --help`、`agent --help`、`mcp --help` 全部 15 秒 harness TIMEOUT；**没有可用 native stdout/exit 证据**。
- 2.28.0 安装版本、V2/V3 agent harness、TUI/classic UI 是三个维度；不能把 bundle metadata 当作 CLI version 结果。
- catalog/schema、native Luna ID/effort、提示符、键序/时序、MCP client registry/回程、SID/resume、rewind、退出码实测均未取得。

官方资料只支撑文档候选：`chat --v3 --agent <owned-name> --model <exact-id> --effort <level> [--trust-all-tools] [--resume-id <bound-id>]`，child environment `KIRO_CHAT_UI=tui`。它不支撑 native profile 的 `Supported`。

## 已实现的边界

`src/kiro.rs` 提供唯一 `KIRO_DESCRIPTOR`：id=`kiro`、binary=`kiro-cli`、无别名/未知 provider fallback、全量 15 facets。

| 七个 Narrow Hook slot | 当前实际状态 |
|---|---|
| H1 CatalogHook | **Unverified**：文档有 JSON 命令，没有 schema/原始目录；不猜字段、不伪造空 catalog。 |
| H2 PlanHook | **Bound**：文档限定 V3/TUI、Fixture-only 的确定性 `LaunchPlan`；Native 请求即使绕过 cloned descriptor 也明确拒绝。 |
| H3 MaterializeHook | **Bound**：单一 owned agent JSON、独占 create、受限 `agent validate` port；失败保留写入 receipt，不覆盖用户配置。 |
| H4 SessionHook | **Unverified**：`/session-id` 输出 grammar/provenance 尚未取得；不扫数据库/最近 session。 |
| H5 SemanticReader | **Unverified**：无 native semantic fixture；不拿 spinner/prompt 当 idle 或结果。 |
| H6 InteractionHook | **Unverified**：无提示符/粘贴/风险确认原始帧及 timing；不伪造通用 `>` matcher 或“2 Enter/500ms”。 |
| H7 ForkHook | InWindow **Unverified**；FullSnapshot/NativeNewSeat 在 descriptor 的 F0 **Unsupported**，无文件复制、无 slash 执行。 |

这不是“7 个原生 Hook 已完成”的声明；五个缺证 slot 明确拒绝，比返回空成功的 stub 更重要。Keystroke、Probe、Teardown 不新增 adapter hook，仍归 K2/K3 公共框架。

### 参数/配置实现

- `OsString` argv 保留模型 ID 与空格/Unicode/引号路径边界；不拼 shell、不向 positional INPUT 塞首条业务消息。
- model/effort 显式携带；`off` 映射无证据则拒绝，不映射成 `none` 或改为 `high`。`--trust-all-tools` 只在显式 bypass 请求时产生，启动风险确认仍未验证。
- `McpStdio` 由 framework 配置真正的 executable/args/env；adapter 不猜尚未存在的 public CLI 参数，也不读取 ambient secrets。
- owned agent name 用长度前缀编码 scope/seat/instance/generation，防 component 拼接歧义。JSON 在 cwd `.kiro/agents/<owned-name>.json`，prompt 用 JSON 正确转义。
- 只列固定三工具 `@team/send_message` / `report_result` / `get_team_status` 和 native builtins；不从其他 Provider 拼 `mcp__...` 名称。
- `includeMcpJson=false`、`includePowers=false` 是计划意图；描述符仍声明 MCP isolation 未验证，不能把 JSON 字段当 native consumed。
- 不设置 HOME/XDG、不调用全局 settings/AI-assisted agent create、不注入 native hooks、不复制认证库、不建立 adapter queue。
- H3 校验路径/name/schema/固定 MCP command，再向受限 OwnedIo 写入；native validator超时/错误/过大输出保留资源，不返回成功。

`classify_exit` 只分类官方定义的 0/1/3/4；0 是 command success 而非业务结果。4 可能发生于 default agent 已执行工具之后。任何退出或 harness timeout 都不抹除 delivery floor、不授权重跑。

## 仍需补齐，禁止自动启用

| R0 项 | 当前状态 / 所需证据 |
|---|---|
| K01 | bundle 已观察；CLI version/help TIMEOUT，需新鲜成功收据。 |
| K02 | V3/TUI flags 仅 DOC；需 exact binary/help/argv native 证据。 |
| K03 | 真实 catalog JSON/schema、Luna ID/effort 未知；禁止换模型绕过。 |
| K04 | agent lookup/validate、user+workspace ambient MCP 排除待证。 |
| K05–K07 | startup risk、paste/Enter/chip、busy/queue grammar 与 bounded timing 未测；没有 InputProfile。 |
| K08 | init/list/call/result/response written/client consumption/presentation 各层原始证据待证。 |
| K09–K10 | local SID、resume、rewind parent/child 与文件不回滚未测。 |
| K11 | process exit/stop/remaining resources native receipts 未测。 |

另外，K3 当前 `JournaledIo` 的 captured grant 只允许 runtime root；Kiro 文档候选 agent 文件位于工作 cwd。**两者不同 root 时，K3 会拒绝，不能靠把 cwd 改成 runtime root 绕过。** 将来集成需由 lifecycle owner 增加经 descriptor/ownership 校验的 WorkingDirectory grant 与相应测试；本 adapter PR 不悄悄修改 K3 的清场权限。跨 Team 环境身份过滤也应由 Host/公共 MCP launcher 完成，不能伪造一个未实现的 server argv。

## 测试口径

`tests/kiro_adapter.rs` 使用明确 Synthetic/Fixture admission 检验纯 argv、carrier report、JSON escaping、exact resume 意图、effort/bypass、H3 partial failure 与 exit 分类，同时验证生产 descriptor、Native H2 与 input slots 保持拒绝。没有假造 catalog、session-id 屏幕或正向 prompt/timing fixture。

Grok 自动化结果见本轮 K4 收据；任何 PASS 都不使缺证项从 Unverified 升级。新的原生运行必须使用 leader 固定候选与授权 caller/被测 Luna；现有 timeout 档案不授权再次本机探测、登录或系统配置修改。
