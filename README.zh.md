[English](https://github.com/Florious95/team-agent/blob/main/README.md) | **中文**

# Team Agent

> 你平时怎么用 Claude Code,现在就怎么带一支团队。

![demo](assets/demo-zh.gif)

## 这是什么

你现在用 Claude Code(或 Codex、Copilot CLI),一次对话只有一双手:它写前端的时候,后端等着;它跑测试的时候,你等着。

装上 Team Agent 之后,还是那个对话窗口,但你可以说:

> "这个活一个人干太慢了,组个队:一个写后端,一个写前端,一个专门写测试。"

然后:

- 屏幕上弹出几个新窗口,每个队友一个,**它们同时开工**
- 队友之间直接互发消息对齐(前端问后端要接口定义,不用经过你)
- 你只跟队长说话,队长汇总进度、有分歧才来问你

不用学习新的编排界面或语言：队长按你批准的方案准备团队文件。会跟 Claude 聊天，就会带团队。

## 安装

```bash
npx @team-agent/installer@latest install
```

然后这样启动(代替你平时直接敲 `claude` / `codex` / `copilot`):

```bash
team-agent claude
```

就这两步。后面发生的一切都在对话里。

## 日常 CLI：start、stop、restart

先核对实际二进制的 `team-agent start --help` 与 `team-agent team --help`。缺少这些入口时，应取得经授权的支持版本，不能仅凭版本号断定功能存在。生命周期命令由合法原生 **Leader 的工具上下文**执行，普通 Worker 不能据此升权。

队长一次性准备 `TEAM.md` 与 `agents/*.md`，明确记录用户选择的 Provider 和布尔型权限绕过策略，例如：

```text
.team/current/
  TEAM.md
  agents/reviewer.md
```

配置完整后，显式选定工作区和团队：

```sh
team-agent start --from .team/current --workspace . --team project-team
team-agent start --workspace . --team project-team
team-agent send reviewer "检查修改并回报主要风险" --workspace . --team project-team
team-agent inbox leader --workspace . --team project-team -n 3
team-agent stop reviewer --workspace . --team project-team
team-agent start reviewer --workspace . --team project-team
team-agent restart reviewer --workspace . --team project-team
team-agent stop --workspace . --team project-team --json
team-agent start --workspace . --team project-team
team-agent team restart --workspace . --team project-team --json
```

| 命令 | 意图与边界 |
|---|---|
| `start` | 完整配置首次建队；后续激活所选非 paused 席位；已确证存活且无配置漂移的实例保持不变 |
| `start AGENT` | 唤醒停止席位、确证存活 no-op，或从完整角色文件加入 RoleOnly 席位；全新 ID 必须明确 Provider/bypass |
| `stop [AGENT]` | 停单席或所选整队，保留会话关联；整队停止返回自有残留报告 |
| `restart AGENT` | 先预检、确认停止后再启动该席位；不偷偷改角色或 reset |
| `team restart` | 明确重启整队，保留显式停止和 paused 席位 |
| `team status` / `status` | 读取简要节点投影；深入诊断用 `doctor` |
| `agent add/remove/reset/clone/fork` | 保留原确认/能力门禁；reset 要求 `--discard-session`，fork 要求 `--as NEW_ID` |

新单席/整队重启默认 `--session auto`：有确证就续接，否则仅允许原引擎支持的合法 fresh。**auto 不是记忆不丢失的保证。** 严格恢复用 `team-agent team restart --session preserve --workspace . --team project-team`；已知无法续接时停止前拒绝。unknown/partial 不是 reset 或盲目重试许可。start/restart 的 `--dry-run` 不产生生命周期效果；没有可操作目标时非零拒绝，不伪报启动成功。

no-op 不表示角色正文/参数修改已生效，也不表示业务就绪；修改运行席位前先 stop。消息已排队不是回包。最终 stop 后核验所选团队残留为空，不能清理兄弟团队或共享服务器。团队目录用 `--from DIR`/明确路径选择；`reviewer.v2` 仍是节点 ID，同名目录歧义用 `agent restart ID` 消除。原生 Provider 不支持的能力必须拒绝，fork 的 `--in-place` 尚未实现。

### 旧命令仅作兼容桥接

既有自动化仍可保留 `quick-start`、`shutdown` 与 `*-agent`，但新日常指引采用上面的直觉命令。旧裸/显式路径 `restart` 保持 preserve 默认和原回执；新 `team restart` 默认 auto。旧单席 start 拒绝运行节点，新 start 可以确证 no-op：不要机械改名脚本或假设 JSON 逐字节等价。详见[操作 Skill](skills/team-agent/SKILL.md)与[操作参考](skills/team-agent/references/team-agent-operator.md)。

## 你可以说什么

组队和管队全是自然语言,举几个真实的例子:

```
"把这个代码库重构成 monorepo,补齐测试。组个队,规划好了给我看。"

"再加一个人专门做 code review,每个合并前都过它一道。"

"前端这个角色干得不行,换个写法重新来。"

"今天先收工。"                  ← 团队关闭,状态保存

(第二天)
"继续昨天的重构团队。"           ← 原班人马带着记忆回来
```

队友不限于写代码的角色。有人用它跑过论文多轮评审(几个评审员互相挑刺再合议)、多角色头脑风暴,甚至让 5 个 agent 自主玩了四轮谁是卧底。你能描述出来的分工,队长就能组出来。

## 队友可以来自不同的 CLI

队长和每个队友各自独立选择用哪个 CLI:

|      | Claude Code | Codex CLI | Copilot CLI |
| ---- | :--: | :--: | :--: |
| 队长 | ✓ | ✓ | ✓ |
| 队友 | ✓ | ✓ | ✓ |

也就是说:你可以让 Claude 当队长出方案、做评审,让 Codex 队友批量干实现。你手上有哪几家的订阅,就能怎么搭着用。

## 原生 CLI 参数路由（默认关闭）

在任意目录按 Provider 全局配置字面启动参数，无需工作区或已登录的 CLI：

```sh
team-agent route set pi -- --mode rpc
team-agent route enable
team-agent route status --json
team-agent route show pi --json
team-agent route add pi -- --verbose
team-agent route clear pi
team-agent route disable
```

默认 OFF；开关与映射保存于 `~/.team-agent/argv-routing.json`。
环境变量 `TEAM_AGENT_CLI_ARGV_ROUTING=1|true|on` 或 `0|false|off` 优先于持久化开关
（忽略大小写和两侧空白），无效值安全关闭。`enable/disable` 不清映射，`set` 精确替换、
`add` 按序追加、`clear` 删除一个映射。结果同时报告持久化态、实际有效态与环境覆盖。

Provider 键为 `claude`、`codex`、`copilot`、`gemini_cli`、`grok`、`pi`。
Claude/Claude Code 共用映射；支持 `claude_code/claude-code` 别名。
第一个 `--` 后全部是字面 argv，包括 `--help`、`--json`、空字符串、`{workspace}`，不会二次 shell 展开。
注入位置为原生 executable 之后、原有参数之前。不要保存密码/token，或覆盖框架管理的
session/MCP/权限参数；既有身份、认证与安全检查不变。

下一次真实 Leader/worker 启动读取当前配置，包括 restart、start/reset/add/clone、Pi 新席 fork
及 remove 失败回滚复活。运行中或仅 attach 的进程、dry-run、model/version/auth 探测与其他辅助进程不变。
已开启但无效的配置会在原生 spawn 前报错；可用 `TEAM_AGENT_CLI_ARGV_ROUTING=off` 紧急停用，
但这不会修复坏文件。Pi `--mode rpc` 只是启动参数，不会自动建立 RPC 宿主或改变 Team Agent 的传输。

## 常见问题

**需要什么前提?**
macOS 或 Linux(含 WSL),装好至少一个上面表里的 CLI,有 tmux(`team-agent claude` 会自动处理,你不需要会用 tmux)。

**队友的窗口我必须盯着吗?**
不用。窗口只是给你随时看一眼的,所有汇报都会回到队长对话里。

**关掉终端团队会死吗?**
不会。团队在后台继续,重新打开对话接着聊就行。

**它和 Claude Code 自带的 subagent 有什么区别?**
subagent 是一次性的、干完即焚、彼此不通信。Team Agent 的队友是持续存在的角色:有自己的记忆、能互相对话、明天还在。

## 状态

`team-agent status` 为每个已登记节点输出九个简要字段：
`name`、`provider`、`model`、`effort`、`runtime_status`、`activity`、`health`、`session_name`、`tmux_command`。
`model`、`effort` 来自最近已接受的启动配置，未设置时为 `null`；不会把待启动的角色编辑或猜测的 provider 默认值当作已生效。
`--json` 与人读输出使用同一信息投影。运行态证据缺失或冲突时显示 `unknown`；非空
`tmux_command` 可复制到已观测的 socket、session、window 和 pane。`--summary` 与
`--detail` 保留兼容性，但不会增加诊断或历史字段。已验收 nodeprobe 的配对方式、
operator-trusted 能力 receipt 与 fail-closed 边界见
[`docs/status-nodeprobe.md`](docs/status-nodeprobe.md)。

**Beta。** 已在真实项目里日常使用,小众配置下可能有毛边。欢迎 issue 和 PR。

## License

AGPL-3.0-or-later,商业授权可联系。
