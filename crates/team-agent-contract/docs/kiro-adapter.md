# Kiro 2.28.0：集成实现与原生证据边界

**当前不是可运行 Kiro 接入，也不是全功能/K5 验收通过。** K1–K4 已组合；参数、物化、原生 helper 解析与有界 read host 可独立验证。Catalog/terminal/session/MCP 的未取得证据不能由 fixture 或 help 代替。

## 本轮物证（替代早期全命令 timeout 的结论）

原始目录：`/Volumes/nvme/tmp/kiro-native-receipts/`；子命令目录 `chat-subcommands/`。由 runner-luna 采集，本开发席未执行 Kiro/登录。

- dispatcher `/opt/homebrew/bin/kiro-cli` 指向 app bundle 的 `kiro-cli`。版本 `2.28.0`、根 help 成功。
- dispatcher `chat --help` 报 `failed to launch .../.local/bin/kiro-cli-chat`。给 PATH 加 bundle 前缀仍失败；不把 PATH 当修复。
- 直接 engine `/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat` 的 `--version`、`--help`、`chat --help` exit 0。
- engine SHA256：`430aae1a4f5ae252e785114d45d61ec465796ad6ca96383fb7ca383dd96b1712`。dispatcher hash 不得用于 engine PID/image fence。
- chat help SHA256：`47e22e67d2d6042ceff70b5d6414ece1a367bbbbd675c873a39d1dd3e868bf20`。支持 `--model`、`--effort`（示例 low/medium/high/xhigh/max）、`--list-models`、list-only `--format plain|json|json-pretty`、`--agent`、`--resume-id`、`--v3`、`--tui`、`--trust-all-tools`。
- `--list` 是 resume-picker 别名，不是 model catalog。
- catalog 调用 180s 外层 timeout、真实 exit 未取得。101586 bytes 输出（SHA256 `7abd2c71c8eeef941f7ae0eb733f358c880f1ab19fcb73fe568d17b566d25599`）**不是 JSON**：byte 0 为 ESC，首尾都是 `Opening auth portal and logging in...` 的 ANSI spinner。不能推断 EOF 等待、有效模型、原生 Luna ID或成功目录。
- agent/MCP help 在首错后 NOT-RUN；没有取得成功的 interactive、SID、rewind 或 MCP 回程物证。

## 已实现的边界

`src/kiro.rs` 提供唯一 `KIRO_DESCRIPTOR`：id=`kiro`、public binary=`kiro-cli`、全量 15 facets；物理 plan 只接受 framework 解析后的 `kiro-cli-chat` engine。旧六家、根 manifest/lockfile 未修改。

### Helper 与有界 catalog read host

`src/kiro/native.rs` 不扫描 HOME，不修改 PATH/软链，不安装组件，不读取环境或凭据。对明确传入的 launcher/home 只检查 canonical launcher sibling、`~/.local/bin/kiro-cli-chat` 和 macOS bundle helper；显式 helper 缺失不切换到另一安装。要求 regular executable，版本不符时拒绝而不 fallback。

`CatalogReader` 捕获完整 read grant（provider/native/executable/cwd/source/bounds），调用前核 cwd identity 与 executable hash。只允许 `chat --list-models --format json`；关闭 stdin、stdout/stderr 字节上限、共享绝对 deadline。公共 command runner 支持数据化 stdout guard，发现已观测 auth marker 后终止**本次自有 child**，返回 `ReadFailure::AuthRequired`。不发登录命令、不重试、不换模型。

**重要限制：** Kiro 自己可能在输出 marker 之前已打开浏览器；关闭 stdin/取消 child 不保证此前无副作用。因此只在操作员授权的 discovery 阶段使用读端口，本轮认证 blocker 解除前不重跑。超时、缺 exit、超限、无效或尾随 JSON 都不能成功；完整 JSON + exit 0 只通过 raw read 边界，仍不等于已知 catalog schema/H1 能力。

### 七个 Narrow Hook slots

| Hook | 当前实际状态 |
|---|---|
| H1 Catalog | **Unverified**：命令语法已知，真实 JSON/schema/模型目录未知；不猜字段。 |
| H2 Plan | **Bound / Fixture-only gate**：确定性 V3/TUI 参数计划；Native 请求不能靠 cloned descriptor 绕过未验证输入/会话契约。 |
| H3 Materialize | **Bound**：owned agent JSON + 独占写入 + 受限 validator；失败保留 receipt。 |
| H4 Session | **Unverified**：无当前 native SID grammar/provenance；不扫描数据库/最近 session。 |
| H5 Semantic | **Unverified**：不把 spinner/prompt 当 idle 或业务结果。 |
| H6 Interaction | **Unverified**：没有当前提示符、paste、Enter/chip/风险确认与 timing fixture；不编造 `>` 或“两个Enter/500ms”。 |
| H7 Fork | InWindow **Unverified**；FullSnapshot/NativeNewSeat 在 F0 **Unsupported**；不复制认证库或执行 slash。 |

Keystroke、三层 Probe、Teardown 不新增 adapter hook，仍由公共 K2/K3 执行。

### 参数/配置与物化

- argv 使用 `OsString`，保持模型精确 ID、路径空格/Unicode/引号。不拼 shell，不把首条业务消息塞 positional INPUT。
- `--model`、五档 `--effort` 与显式 bypass `--trust-all-tools` 按 help 构造。`ultra` 拒绝，不降成 max；模型是否支持某 effort 仍由真实 catalog 决定。`--format` **仅用于 list**，不加入 interactive plan。
- `McpStdio` 由 framework 提供 executable/argv/env；adapter 不猜未落地的 public CLI 参数或 ambient secrets。
- cwd `.kiro/agents/<owned-name>.json` 的名称含 scope/seat/instance/generation 无歧义长度编码。prompt 用 JSON escaping。
- 固定三工具及 builtins，`includeMcpJson=false` / `includePowers=false` 仅为计划意图，不冒称 native consumed/isolation 已证。
- K3 `JournaledIo` 按 descriptor 及 seat 捕获的 resource-kind grant 允许 WorkingDirectory；绝不把任意绝对路径变成权限。
- `ScopedMaterializer` 对已捕获 cwd/root identity、每个路径分量 no-follow、独占0600文件、必要0700子目录、文件 creation identity/hash 做校验。现有目录不 chmod，不覆盖文件，不递归删除 shared cwd；确认 quiescent 后只删除本次 receipt 对应原 inode/bytes。
- H3 校验命令仅 `agent validate <owned-path>`。其 native 语法/行为仍待独立 help/实机验证，不能因公共 runner 能执行就升级原生 admission。

`classify_exit` 保留官方定义的 0/1/3/4：0 不是业务结果；4 可能出现在 default agent 已发生 effects 之后。`command_failure` 另区分 observed wrapper launch failure、AuthRequired、true timeout、output limit 与 native exit124；所有分类不降低 delivery floor、不授权 replay。

## 未完成项 / K5 准备门

认证前置由用户负责；不得读 credential store 或自动 login。认证成立后仍须固定 helper identity、真正 catalog/schema 与 Luna 模型/effort；若无 Luna 则有限 native case NOT-RUN，不能换 Claude。

生产 H4/H5/H6/H7、动态 tmux target 与 K3 持久化接线、共享 supervisor 的实进程入口、MCP 子进程绑定、macOS candidate 与完整盲测用例仍须按实际实现/原始证据关闭，不能把 library 组合写成终端端到端完成。三层探针、F0–F5、物理 executor 的已有单测不等于 Kiro 集成通过。

## 自动化口径

- `kiro_adapter.rs`：受控计划/载体/JSON/资源/effort/bypass/exit，包括生产 admission 拒绝。
- `kiro_dispatch.rs`：受控 bundle/local/helper 文件布局、直接 engine、版本、dispatcher 分类。
- `kiro_catalog.rs`：auth spinner、受控大 JSON、真实 EOF、真实自有 subprocess guard、timeout/exit/截断、captured grant/image 拒绝。不调用真实 Kiro。
- `resource_grants.rs`：共享 cwd grant/no-follow/exclusive/token/dirty/foreign/cleanup。

Cargo/fmt/clippy 仅 Grok/授权 CI；真实 source/tree、实际用例数、退出码与清理以构建收据为准。不把新增未执行测试或任何 fixture PASS 写成原生 PASS。
