[English](https://github.com/Florious95/team-agent/blob/main/README.md) | **中文**

# Team Agent

> 说一次。一个 team 跑起来。

为 Claude Code 和 Codex CLI 设计的多 agent 运行时。**编排由 lead 自己完成**——你用自然语言描述目标,它跨厂商组建团队、执行工作、汇报结果。

没有 DAG。没有 YAML。没有 Kanban。只有对话。

```bash
npx @team-agent/installer@latest install
```

**重要:** 主 Agent，也就是 lead 所在的 Claude/Codex 对话，必须运行在
tmux 管理的 pane 里。最省心的方式是 `team-agent claude` 或
`team-agent codex`；你自己已有的 tmux/Ghostty/Finder 分屏布局也可以。普通非
tmux 终端不够，因为队员向 lead 回传消息时必须有一个明确、可验证的 pane 目标。

---

## 为什么做这个

你有 $20 的 Claude 订阅。月中 18 号,额度用完了,但还有活要交。

你可以升级到 $200(贵)。可以切到更便宜的模型(损失 Claude 的品味)。可以等 1 号重置(影响工作)。

或者你可以让 Claude 继续当 lead——梳理任务、协调、验收——把执行外包给 Codex 或者第三方 API 接入的 Claude。同一个对话,十分之一的成本,质量不下降。

这是这个工具的用法之一。**Lead 还是 Claude,执行者可以是任何人。**

---

## 它具体在做什么

装一次。然后在任何 Claude Code 或 Codex CLI 对话里说类似这样:

> "做一个小型 SaaS 来追踪客户反馈——前后端、测试、验收标准都要。"

Lead 自己想清楚:

- 需要哪些角色,每个角色的定义应该多丰富
- 哪个角色用哪个 provider(Claude 写前端、Codex 写后端逻辑、第三方 API 跑测试节省成本)
- 队员之间怎么通信(P2P + 共享任务列表)
- 什么决策需要回来问你

然后它 spawn 队员、跑工作、汇报。**你全程在同一个对话里**。队员在独立终端窗口里跑,你可以随时切过去看每个人在做什么。

关电脑回家。明天打开 Claude Code 接着说一句"继续昨天那个 team",team 还在,session 状态完整,从昨晚那里接着干。

---

## 和现有方案的差异

市面上已经有几个好的多 agent 工具,各自选了不同的 trade-off:

|                              | 形态                | 你要配置的东西                 | Lead                                | 跑在哪里        |
| ---------------------------- | ------------------- | ------------------------------ | ----------------------------------- | --------------- |
| **agent-teams-ai** (871★)    | Electron 桌面应用   | UI 里写 roles + provisioning prompt | "CTO" 看 Kanban                   | 桌面应用        |
| **omo** (54.9k★)             | OpenCode 插件       | `ultrawork` 命令词             | Sisyphus,角色固定                  | OpenCode TUI    |
| **CCB** (2.5k★)              | CLI + TOML          | 每个 team 一份 `.ccb/ccb.config` | 无(你自己组队)                   | tmux            |
| **ClawTeam** (3.3k★)         | CLI + prompt 注入   | TOML team 模板                 | 无                                  | tmux + Web UI   |
| **Team Agent**(本项目)      | MCP 运行时          | 不需要配置                     | 你本来就在对话的 Claude/Codex       | 你现有的终端    |

本项目的 lead **不是**一个有特定 personality 的 "orchestrator agent",**就是** Claude(或 Codex)——你平时聊天用的那个,加上 spawn 和管理队员的能力。

为什么这件事重要:

- **编排能力随模型能力提升**,不随框架 feature 增加。Claude 5 一出,lead 自动变聪明,不需要我发版。
- **Lead 可以为任何任务组建任何 team**,不限于预设的开发角色。我们已经跑通了学术论文修订、多角色头脑风暴、对抗类游戏("谁是卧底"4 局连测)——这些场景都不是预编程的,**全部是 lead 在对话中现场组建出来的**。
- **你放弃了编排 UI,换来"交付你原本无法精确描述的工作"的能力**。

---

## 工作原理(简要)

三件事让它能跑起来:

**1. 编排层就是 lead。** 没有外部 workflow engine。Lead 自己推理角色定义、通过 MCP 调度、根据你的对话实时调整 team。运行中加队员、改角色、解散 team——全部用自然语言。

**2. Transport 是基础设施,身份是持久的。** 队员是长寿命的 `claude` 或 `codex` 子进程,有稳定的 session ID。窗口挂了运行时自己拉起来恢复状态——**整个过程不进入任何 agent 的上下文**。身份在 system prompt 层注入,不是塞在对话历史里的脆弱 hack。

**3. 用标准协议,不发明协议。** 用 MCP 做工具调用,用 Skill 文件做角色定义。生态发什么,本项目自动获得什么。

完整设计哲学和边界,见 [`docs/team-agent-foundation-and-boundaries.md`](./docs/team-agent-foundation-and-boundaries.md)。

---

## 快速开始

### 安装

```bash
npx @team-agent/installer@latest install
```

会自动设置 MCP server、注册 Team Agent skill、写入 Claude Code / Codex CLI 配置。

源码安装:

```bash
git clone https://github.com/Florious95/team-agent.git team-agent
cd team-agent
npm exec --yes --package . -- team-agent-installer install
```

### 使用

在 tmux 内启动 lead。下面两个快捷命令会在需要时创建或附着到 tmux leader
session:

```bash
team-agent claude
team-agent codex
```

如果你已经有自己的 tmux/Ghostty/Finder 分屏布局,可以继续用;硬要求只是可见的
lead 对话必须有 tmux pane。然后在对话里:

```
你:    我想重构这个代码库,拆成 monorepo,加测试覆盖。帮我规划并执行。

Lead:  [提议一个 team:重构架构师 (Claude)、代码搬运工 (Codex)、
        测试作者 (Claude)、reviewer (Codex)。把 trade-off 说清楚后等你确认]

你:    开始。
```

完事。队员窗口出现,lead 推进工作、需要决策时停下来问你,你说"关掉"就关掉。

### 关闭 / 恢复

```
你:    先把 team 关了。
Lead:  [保存状态、关闭 pane,约 2 秒]

(第二天)

你:    继续昨天那个重构 team。
Lead:  [从保存的 session 恢复队员,约 2 秒,同样的上下文]
```

---

## 当前已验证的能力

在多种真实工作流下验证过:

- **跨厂商混合 team**——Claude 当 lead,Codex 实现,第三方 API 接入的 Claude 跑测试
- **Web 开发 team**——5 角色:前端 / 后端 / 契约 / 需求分析 / 测试
- **学术协作**——5 阶段论文修订工作流,审稿人和研究者对抗 + 共识机制
- **博弈 / 实验**——4 局"谁是卧底"实验(1 lead + 4 玩家全自治,意外跑出了关于 LLM theory-of-mind 的真实观察)
- **Emergent 恢复**——lead 自己识别 Codex 权限确认 prompt 并按 enter;窗口关闭后第二天接着用
- **对话驱动的 team 变更**——运行中加队员、改角色、解散 team,全在 lead 的对话里完成

---

## 支持的 lead 和 teammate

| 角色     | Claude Code(订阅制) | Claude Code(第三方 API) | Codex CLI |
| -------- | -------------------- | ------------------------ | --------- |
| Lead     | ✓                    | ✓                        | ✓         |
| Teammate | ✓                    | ✓                        | ✓         |

任何队员可以用和 lead 不同的 provider/tier。运行时分别处理每个队员的认证、session 生命周期、resume。

---

## 项目状态

**Beta**。已经在真实工作流里跑通,但不常见的配置可能有粗糙的地方。欢迎 issue 和 PR。

## License

AGPL-3.0-or-later。商业 license 可联系协商。
