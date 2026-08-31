---
name: Pi provider 适配
type: requirement
status_at: 0.5.57
source_signed: 2026-08-31
---

# Pi provider 适配

本页是 Pi 专属条款的落位地图，不是第二套需求正文。权威句子写在对应功能页；本页只回答“哪一页管哪一条”。

用户会签原文：`.team/artifacts/pi-provider-adaptation/requirements/pi-provider-requirement-final.md`（状态：用户已会签，可供需求维基管理员入库）。

本文不建立新的通用 Provider schema；共享字段仍以现有 Team Agent 角色需求为权威。

## 条款落位

| 定稿条款 | 权威页 |
|---|---|
| Pi 可以作为 Team Agent leader 或 TeamMate | [[F3 Provider、模型、权限与资源组合]] |
| 已由其他 Provider 建立的团队可以添加 Pi TeamMate | [[F3 Provider、模型、权限与资源组合]]、[[F2 角色、成员与能力编排]] |
| 同一团队可以运行多个 Pi 席位，席位身份不得串位 | [[F3 Provider、模型、权限与资源组合]]、[[F7 会话身份、续作、克隆与分叉]] |
| 通过 Team Agent 启动 Pi，应尽量等同于用户直接运行 `pi`；共享原生配置、认证、插件和默认行为；Team Agent 只增加协作所必需的每席位 MCP 身份和会话隔离 | [[F3 Provider、模型、权限与资源组合]]、[[F7 会话身份、续作、克隆与分叉]] |
| `team-agent pi` 与 Pi TeamMate 角色在省略 model 和 effort 时可以启动，并使用 Pi 的原生默认值；不得因此合成 model/effort 参数；用户显式提供的合法 model/effort 应原样传递；非法值按 Pi 能力明确拒绝，不得静默改写 | [[F3 Provider、模型、权限与资源组合]]、[[F2 角色、成员与能力编排]] |
| Pi 既可从普通可寻址 tmux pane 启动，也可通过 Team Agent 团队生命周期启动；用户无需理解或操作底层 tmux socket | [[F9 显示、平台、安装与黑盒体验]] |
| npm、Homebrew、WSL 等标准安装形成的 Pi 可执行文件或符号链接均应可用；可执行文件解析遵循 shell 的 PATH-first 语义；不得越过 PATH 中首个可执行 `pi` 选择后置私有 wrapper；也不得要求用户额外提供 Team Agent 专属的 verified wrapper | [[F9 显示、平台、安装与黑盒体验]] |
| Pi 沿用已有共享角色字段及权限语义，包括 MCP、tools 和 `dangerously_skip_permissions`；不为 Pi 发明第二套字段、别名或 `auto`/`skip` 等不同表达 | [[F3 Provider、模型、权限与资源组合]]、[[F2 角色、成员与能力编排]] |
| Pi 与 adapter 的版本号、包版本和 digest 可以作为诊断信息；当所需协议与能力满足时，不得以精确版本或 digest 作为 Provider 准入门槛；本条不影响发布物自身的版本、SHA 和供应链完整性核验 | [[F3 Provider、模型、权限与资源组合]]、[[F10 治理边界与黑盒验收]] |
| 在已有有效 owner/receiver 的团队中添加 Pi，不得要求重复 claim 团队 | [[F1 对话开团与团队生命周期]] |
| restart/resume 只在持久 backing 存在且身份匹配时恢复原会话；缺失或不匹配时 fail-closed；start-agent reconstruction 与 resume 是不同语义，不得把重建冒充恢复；reset 丢弃旧会话并建立 fresh 会话，且不得影响 sibling 席位 | [[F7 会话身份、续作、克隆与分叉]]、[[F2 角色、成员与能力编排]] |

## 非目标与禁止项

以下机制没有独立需求依据，不得作为 Pi 适配的默认设计。分域禁令同时写在对应功能页。

- 强制用户填写 model 或 effort → [[F3 Provider、模型、权限与资源组合]]
- 从团队级默认值重新合成 Pi model/effort 参数 → [[F3 Provider、模型、权限与资源组合]]
- 用精确 Pi/adapter 版本或 digest 建立准入硬门 → [[F3 Provider、模型、权限与资源组合]]、[[F10 治理边界与黑盒验收]]
- 扫描完整 PATH 并绕过首个 `pi` → [[F9 显示、平台、安装与黑盒体验]]
- 要求 Team Agent 专属 wrapper 身份 → [[F9 显示、平台、安装与黑盒体验]]
- 为 TeamMate Pi 建立独立的原生配置、认证或插件环境 → [[F3 Provider、模型、权限与资源组合]]
- 添加无需求依据的 `--no-*` 禁用参数 → [[F3 Provider、模型、权限与资源组合]]
- 将 current-client 或重复 claim 作为 Pi 启动前置条件 → [[F1 对话开团与团队生命周期]]
- 用 ownership/current-client workaround 掩盖 wrapper 或 materialization 根因 → [[F1 对话开团与团队生命周期]]
- 把 tmux 内部操作变成普通用户必须学习的产品流程 → [[F9 显示、平台、安装与黑盒体验]]

## 与现有需求的关系

- 角色与动态成员：继承 [[F2 角色、成员与能力编排]]、[[F3 Provider、模型、权限与资源组合]]；只补充 Pi 的 Provider 专属行为。
- 身份与会话：继承 [[F7 会话身份、续作、克隆与分叉]]；明确 Pi 每席位隔离及 resume/reset 边界。
- 黑盒体验与安装：继承 [[F9 显示、平台、安装与黑盒体验]]；“支持普通 tmux pane”不等于要求用户管理 tmux。
- 发布完整性：继承 [[F10 治理边界与黑盒验收]]；adapter 诊断信息不得与发布物 SHA 核验混为一谈。
- 团队生命周期：继承 [[F1 对话开团与团队生命周期]]；已有团队添加 Pi 不重新认领 owner。
