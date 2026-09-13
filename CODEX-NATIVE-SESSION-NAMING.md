# Codex 子节点原生会话命名设计

状态：**Proposed / 尚未实现**。本提交只增加设计说明，没有修改运行时代码，也没有执行或宣称通过 Rust、tmux、Codex 实机测试。合入这份说明本身不会修复命名问题。

调查基线：`24d6eb13ce914b598a52a9dcecfb8cd46f988c32`（2026-09-13 读取的 `main`）。实现前应重新核对调用链与当前 Codex 版本；不得把上游源码快照当作本机已验证的兼容性承诺。

## 1. 用户目标与非目标

当 Team Agent 启动一个名为 `codex` 的 Codex 子节点时，**该子节点对应的 Codex 原生会话名称应为 `codex`**，效果等同于在该会话里执行 `/rename codex`。

名字来自最终子节点身份，不来自 provider 字符串：名为 `reviewer`、provider 为 `codex` 的节点应被命名为 `reviewer`，不能把所有 Codex 会话都改成同一个 `codex`。

需要区分四个概念：

| 概念 | 本次是否修改 |
| --- | --- |
| Team Agent 的 agent ID、路由身份和 MCP 身份 | 不修改；作为期望名字的来源 |
| tmux session/window/pane 的名称或标题 | 保留现有行为；不作为修复成功的证据 |
| 启动命令、进程标题、首条 prompt | 不用于冒充原生命名 |
| Codex 原生 thread/session 的可持久化名称 | 本次要初始化的对象 |

不通过修改 Corral/App 的展示兜底掩盖上游未命名，不让模型执行改名，不直接写 Codex 的数据库、rollout 或 `session_index.jsonl`。不增加新的命名守护进程，不为改名另建一条替代会话，不自动改名用户已在使用的历史会话。

## 2. 已确认的根因与调用链

### 2.1 整队冷启动只设置了外部名字

`crates/team-agent/src/lifecycle/launch/spawn.rs::spawn_agents` 已传入 `ProviderCommandContext.agent_id_hint = Some(agent_id_raw)`。

该处注释明确说明：Claude/Copilot 可以将 hint 转换为 `--name`，Codex 没有对应的已接入参数，并忽略 hint。随后创建 `WindowName::new(agent_id_raw)`，这只命名了 tmux 窗口。

spawn 后现有流程调用 `adapter.handle_startup_prompts(...)`，必要时启用 fast mode，然后收集 `StartedAgent`；这里没有 Codex 原生改名阶段。

`provider/adapters/codex.rs::codex_base_command` 负责 model、sandbox、profile、developer instructions 和 MCP 参数，没有原生会话命名逻辑。

### 2.2 动态添加不能只靠修复冷启动覆盖

`lifecycle/launch/add_agent.rs` 的模块边界明确规定：起席走 `restart::start_agent_at_paths`，不是直接调用冷启动的 `spawn_agents`。

`lifecycle/restart/agent.rs::start_agent_at_paths` 调用 `restart/common.rs::spawn_agent_window`，再登记启动状态、重投待发消息并启动 coordinator。因此，不能在 `start_agent_at_paths` 返回后才尝试向终端发送 `/rename`：此时第一条业务消息可能已经投递。

`spawn_agent_window` 同样构造带 `agent_id_hint` 的 `ProviderCommandContext`，并根据 `resume_session_id` 选择 fresh 或 resume command plan。这是另一个必须覆盖的创建入口。

### 2.3 不顺带开放 Codex fork

当前 `lifecycle/launch/fork_agent.rs::in_window_fork` 只为已验证的 Grok/Claude subscription 组合返回原生窗口内分身命令；Codex 返回未验证/不支持。此 PR 不应借命名修复顺带开放 Codex fork。

若 clone、reset、rebuild 等操作最终创建的是新的 Codex 会话，应在它们实际经过的 fresh 启动路径初始化名字；必须由生命周期测试证明覆盖，而不是只测试一个帮助函数。真正 resume 原有会话则采用下面的保留规则。

## 3. 原生接口证据与限制

OpenAI 的 CLI 文档规定 `/rename <name>` 更新保存的会话名称，不改变 transcript；它与终端标题设置不是同一概念。

App-server 文档提供 `thread/name/set`，用于设置指定 thread 的名称，并通过 `thread/name/updated` 通知客户端。该接口是已有**准确原生绑定**时可采用的原生控制通道，不代表 Team Agent 现有的普通 Codex TUI 子节点已经具备该通道。

现有 `crates/team-agent/src/codex_app_server.rs` 主要处理有明确 Unix socket/thread 绑定的 leader 投递。不得直接拿 leader 的 socket/thread 去给 child 改名，也不得为寻找一个可改名的 thread 使用 `--last`、最近文件、工作目录或名字匹配。

上游 `a592c38c16cdd7623dacc9168926ebccedfb67d3` 的 `tui/src/chatwidget/session_flow.rs` 明确说明 `on_thread_name_updated` 静默更新名称元数据。因此，不能编造或固定等待一个未经验证的 `Session renamed ...` 成功字符串。

上游同一快照的 `tui/src/status/card.rs` 会在存在名称时显示 `Thread name` 字段。但这只是实现观察，不是跨所有 Codex 版本、终端宽度和语言的稳定解析承诺；必须通过所支持版本的 fixture 与实机测试确认。

参考：

- [Codex CLI slash commands](https://developers.openai.com/codex/cli/slash-commands)
- [Codex app-server](https://developers.openai.com/codex/app-server)
- [原生名称通知处理](https://github.com/openai/codex/blob/a592c38c16cdd7623dacc9168926ebccedfb67d3/codex-rs/tui/src/chatwidget/session_flow.rs)
- [原生 status 名称字段](https://github.com/openai/codex/blob/a592c38c16cdd7623dacc9168926ebccedfb67d3/codex-rs/tui/src/status/card.rs)
- [冷启动入口](https://github.com/Florious95/team-agent/blob/24d6eb13ce914b598a52a9dcecfb8cd46f988c32/crates/team-agent/src/lifecycle/launch/spawn.rs)
- [单席位启动入口](https://github.com/Florious95/team-agent/blob/24d6eb13ce914b598a52a9dcecfb8cd46f988c32/crates/team-agent/src/lifecycle/restart/agent.rs)
- [共享命令构造与 spawn](https://github.com/Florious95/team-agent/blob/24d6eb13ce914b598a52a9dcecfb8cd46f988c32/crates/team-agent/src/lifecycle/restart/common.rs)

## 4. 设计决策

### 4.1 由 command plan 显式声明启动后的原生动作

建议增加 provider 层的类型化启动动作，而不是在 transport 中根据命令字符串猜 provider，也不是在全局变量中缓存“下一个要命名的节点”。下面是接口草图，不是已经存在或可直接编译的代码：

```rust
pub enum NativeStartupAction {
    SetSessionName { name: String },
}

// CommandPlan 新增字段，所有现有构造器默认空列表。
pub post_start_actions: Vec<NativeStartupAction>;
```

Codex 的 `build_command_plan` 在 fresh 且 `agent_id_hint` 非空时追加 `SetSessionName`。实际命令 argv 不追加不存在的 `--name` 参数，也不把 `/rename ...` 作为初始模型 prompt。

Codex 的 `build_resume_command_plan` 不追加这项动作。没有名字请求的通用调用保留原有行为；其他 provider 默认空动作，既有 Claude/Copilot 命名机制不变。

provider 层负责原生命令、协议、响应识别及类型化错误；lifecycle 层负责何时执行、具体目标身份、输入锁、失败回滚和何时放行业务消息。状态变更仍通过既有 repository/projection 权威，不新建直接读写 runtime state 的旁路。

### 4.2 新会话命名，旧会话保留

| 实际会话操作 | 命名策略 |
| --- | --- |
| 整队冷启动、新增节点、新建 clone | 对新会话初始化一次最终 agent ID |
| reset 或显式允许的 fresh-after-missing-rollout | 确认新会话后初始化一次 |
| resume 原会话 | 保留原生名称，尤其保留用户手动 `/rename` 的结果 |
| 存活节点的 Noop / 重复 start | 不发送命名命令 |
| 现有未命名历史会话的批量修复 | 不在本次范围内；不能隐式覆盖历史 |
| 当前不支持的 Codex fork | 保持不支持；不借本次命名修复放开 |

名称不是唯一身份。两个团队可以有同名节点；任何恢复、读回和确认必须使用准确 thread ID/启动绑定，而不是按名字恢复。

### 4.3 启动次序必须形成屏障

```text
预检并验证期望名字
  → 构造 command plan
  → 创建该子节点的进程/pane
  → 核对本次 spawn 的 pane、session/window、transport endpoint 和存活性
  → 沿用已有启动提示处理
  → 确认原生会话可接受本地命令
  → 在输入锁内执行并确认原生命名
  → 继续既有 fast mode / 状态登记 / ready 流程
  → 才允许重投待发消息或投递第一条任务
```

屏障必须位于两个实际创建入口中。不能只在 UI 打开后、首次 report_result 后、coordinator 的日常扫描中，或 `start_agent_at_paths` 返回后补做命名。

同一节点上的命名和业务投递必须串行。锁获取失败时不能照搬某些既有路径的 `acquire_or_proceed` 降级行为继续注入；命名必须失败关闭。不同节点不得通过新的全局锁互相串行等待。

## 5. 普通 TUI 子节点的执行方案

普通 TUI 子节点优先使用其自身的 `/rename <name>`，不更换 CLI 运行模式，不读取或改写 Codex 私有落盘。

### 5.1 就绪判定不能复用宽松 banner 判定

现有 `provider/startup_prompt.rs::classify_codex_startup_screen` 将 `OpenAI Codex` 等视为 ready 线索。这足以服务其现有提示处理，不足以证明可以安全发送原生命令。

新增命名执行器必须通过经过 fixture 验证的输入框/本地命令可用状态确认就绪，并排除 trust/update/login 弹窗、MCP 初始化、执行中状态、已有输入和 shell。仅发现 banner、历史中的 `›`、工作目录名或 tmux 标题，都不能放行。

不得为了通过就绪判定扩大自动批准权限、绕过登录或自动点击原本不会处理的安全提示。判不出来应返回明确错误，不猜测。

### 5.2 注入与确认

只向本次 spawn 返回且已核对身份的 child pane 发送一次纯文本原生命令：

```text
/rename <最终 agent ID>
```

名字不是 shell 命令，不做 shell 引号包裹；使用 transport 的字面文本机制。拒绝空值、换行、控制字符和其他会改变输入边界的字符。合法的中文、空格和标点必须由测试证明不会改变名称本身或产生第二条输入。

审查 `InjectPayload` 的实际行为：不能引入业务消息 envelope、message-ID 前缀、补充说明或多行 prompt，使 `/rename` 退化为普通模型消息。现有窗口内 fork 使用 `TextSkipConsumptionPoll` 可作为参考，但不能未经测试就认为它保证了改名已消费和已持久化。

发送后必须区分“字节已写入”“原生命令已消费”和“名称已确认”。不得重复粘贴命令；不得以不断补发 Enter 替代确认，否则可能提交后续业务消息或用户输入。

对没有 app-server 控制绑定的 TUI，建议用同一会话的原生 `/status` 名称元数据作为读回途径：先确认 rename 输入已消费，再发送 status；只解析本次产生、结构完整的原生状态结果，核对 native session/thread 身份与精确名称。不能从整个 scrollback 搜索一个同名字符串，也不能接受旧 status 卡片或输入回显。

**实现门槛：**需在目标 Codex 版本验证 rename 的确认/持久化时序、status 字段与身份字段、窄终端换行和输入消费状态。尚未验证的组合必须明确标为不支持/未确认，不能默默报成功。若这一读回途径不能可靠满足门槛，不能发布一个仅有屏幕 substring 判断的替代实现。

### 5.3 已有准确 app-server 绑定时

若将来某类 child 已经具有属于它自己的、经过核对的 native app-server 绑定，可以实现同一 provider 接口的协议后端：

```text
initialize / initialized
  → 对准确 child thread ID 执行 thread/name/set
  → 检查对应 request ID 的成功响应
  → 通过原生 thread 元数据读回准确名字
```

协议字段和响应形状以已验证版本的官方 schema 为准；读回不得请求 turns/transcript。沿用该 child 的有效 profile、环境和 endpoint，不能误用父节点绑定或默认用户的另一份 CODEX_HOME。

这不是 TUI 路径失败后的盲目兜底：没有准确绑定就不能尝试命名。不得 spawn 一条新 thread 然后把它冒充正在运行的 child。

## 6. 失败、重试和可观测性

建议使用类型化结果/错误，至少区分：`not_requested`、`preserved_existing`、`confirmed`、`unsupported`、`invalid_name`、`identity_mismatch`、`input_lock_timeout`、`not_ready`、`transport_failed`、`confirmation_timeout` 和 `native_rejected`。这些是拟议语义，不是本提交已添加的枚举。

`sent` 或超时后的 `unknown` 不能转换为 `confirmed`。出现不确定结果时不能立刻再发一遍 rename，也不能用改 tmux 标题或直接写索引“补齐成功”。

新建路径的命名失败必须阻止该节点的首条任务。具体实现沿用启动事务回滚：只允许回收本次创建、身份仍匹配且尚未交付任务的 pane；不得杀父节点、其他节点、被替换的新一代 pane 或 resumed 会话。回收失败必须连同原始错误显式返回。动态 add 的 reservation/spec/state 回滚与 pane 清理必须一起验证，不能返回错误后留下未登记的孤儿进程。

批量冷启动还需测试部分成功/部分失败：已经登记的节点、仍待登记的节点和失败节点必须有明确归属，不能以丢失状态记录的方式维持假成功。

只有确认完成才记录原生命名成功。日志仅包含 team/agent ID、provider、本次 spawn 身份、精确 native ID（可取得时）、期望名字、结果码和耗时；不记录 pane 正文、业务 prompt、完整 argv、profile 内容或凭据。

初始化只属于本次新会话的生命周期。后续刷新、健康采样和正常消息收发不得再次改名；用户之后使用 `/rename` 的结果必须保持，不建立持续“纠正”用户命名的循环。

## 7. 建议的运行时代码修改位置

| 文件/模块 | 预期变更 |
| --- | --- |
| `provider/types.rs` | 为 command plan 增加默认空的类型化 post-start action；补充名字 hint 的语义 |
| `provider/adapter.rs` | Codex fresh plan 将最终 hint 转为原生命名动作；resume 不追加；提供 provider 执行入口 |
| 新增 `provider/session_name.rs`，在 `provider/mod.rs` 注册 | 名字验证、原生命令/协议、确认状态机和错误；不承担生命周期或直接保存 runtime state |
| `lifecycle/launch/spawn.rs` | 冷启动执行命名屏障；更新“Codex 忽略 hint”的过时注释；正确处理失败 pane |
| `lifecycle/restart/common.rs` | 共享新建入口执行同一屏障；resume 保留；不把控制命令发送推迟到业务重投之后 |
| `lifecycle/restart/agent.rs` 及相关启动事务 | 核对状态登记、待发消息重投和回滚次序；仅在确有需要时修改，不重复实现命名逻辑 |
| provider 与 lifecycle 测试模块 | 纯函数、假 transport、真实入口覆盖和隔离 Codex 原生验收 |

实现前应检查其他 fresh command-plan 消费者，避免新增字段却无人执行；也要更新相关 struct literal 和 golden，不能为了测试全绿删除身份/启动次序断言。遵守 `DESIGN-RULES.md` 的依赖方向、状态写权威和行数门禁，不扩大旧的大文件债务。

## 8. 测试与验收

下表均为**待实现/待执行**，没有一项在本设计提交中被宣称通过。

| 层级 | 必须证明的行为 |
| --- | --- |
| 纯函数 | `codex` → `/rename codex`；`reviewer` 不变成 provider 名；中文、空格、标点按字面保留；控制字符/换行被拒绝 |
| Command plan | fresh + hint 才追加命名动作；缺少请求不变；resume 没有动作；其他 provider 行为不变 |
| 就绪状态机 | banner-only、shell、trust/update/login、忙碌、初始化和已有输入不会触发命令 |
| 确认状态机 | 输入回显、旧 status、错误 native ID、部分/换行响应和仅写入成功不能伪造 confirmed |
| 输入顺序 | 命令只注入一次；rename 已消费后才允许 status；确认完成后才允许首条业务消息 |
| 锁与超时 | 锁冲突不降级注入；所有等待有统一上限；异常能释放锁；失败不重复补发 Enter |
| 生命周期 | 整队冷启动与动态 add 两条真实入口均覆盖；fresh reset/rebuild/clone 按实际路由覆盖 |
| 保留行为 | resume 和 Noop 均不覆盖手动改名；不开放当前不支持的 Codex fork |
| 并发/身份 | 同 cwd 同时创建多节点不串名；父节点不被改名；跨团队同名不串 thread；不同 endpoint 不串 pane |
| 回滚 | 新建失败没有孤儿 pane/虚假 running；pending 消息保留且未提前发送；清理失败有明确错误 |
| Profile | 使用实际 child 的 profile/环境；不切换 HOME/CODEX_HOME，不污染其他用户或其他会话 |
| 原生实机 | status/read 显示原生名字；退出后按精确 native ID 恢复仍保留名字；之后手动 rename 不被改回 |
| 无模型副作用 | 命名不启动模型 turn、不消耗生成 token，不把 `/rename`、`/status` 或名字当业务 prompt |
| 回归门禁 | `scripts/gate.sh` 及相关 Rust 测试通过；门禁保持原断言，不以缩减覆盖换全绿 |

实机验收应在独立临时 workspace、独立 tmux endpoint 和独立 Codex 配置中执行。记录 Codex 版本、平台、终端宽度、创建路径、期望名字、实际 native ID/名字及恢复后的结果；不使用真实任务正文或真实用户凭据作为 fixture，进程测试声明仓库要求的 hermetic boundary。

## 9. 合并与交付标准

本设计 PR 可以作为实现依据，但不能标为命名 bug 已修复。后续运行时实现必须同时满足：

1. 至少整队冷启动与动态添加子节点能通过原生路径设置正确名称，而不只是 command-plan 单测通过。
2. 首条任务的正确性、resume 的手动名称、父子身份隔离及失败回滚有可复核的测试。
3. 明确列出实际验证过的 Codex 版本和平台；未运行的实机项目如实标注，不将 mock/golden 结果替代原生持久化验证。

全过程不要求合并、部署或发布包；本设计提交不包含这些操作。
