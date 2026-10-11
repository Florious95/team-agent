# K2 — 唯一物理执行器与宿主边界

K2 在 K1 的纯契约之上增加 Host/物理交付/探针采集。**没有 Kiro adapter/profile、CLI、store、MCP server 或 K3 生命周期事务**；真实 tmux 测试的被测进程是受控 Python TTY fixture，不是 Kiro，也不是订阅 Agent。

## 模块与调用链

- `host::command`：结构化 executable/argv/env + 剩余 budget；非阻塞 stdin/stdout/stderr，有输出上界，无无界 reader-thread join。命令进程退出和输出采集终态分开；只终止本次直接创建的 command child，未 reap 的 handle 显式保留。
- `host::files`：私有目录的 dev/inode/owner receipt、no-follow/openat 文件访问、exclusive create、同一 native-input 文件锁。锁冲突 defer，不接管“旧锁文件”，也不软锁告警后继续。
- `host::process`：Linux `/proc` start-tick + boot/image identity；macOS 安全 libproc 的 birth sec/usec + image。后续采样核 birth、parent 与 image metadata，不把 PID/进程名相同当同实例。
- `host::shell` / `host::tmux`：新私有 endpoint、独立 config、byte-preserving POSIX quoting 与 owned wrapper；不导入旧 Provider-aware executor。wrapper 先按名称清除继承的 `TEAM_AGENT_*` 再应用显式 captured overlay，不打印这些变量值，不清 HOME/XDG/auth 配置；`exec` native 后 `pane_pid` 是实际 native PID；`remain-on-exit` 在 spawn 前配置，退出由 tmux 的 wait facts 证明，不用 inert shell 或 terminal 文本代替。
- `runtime::delivery`：`deliver_envelope → inject_with_contract`，唯一 paste/key 状态循环。NativeControl 和 StartupAck 也用此循环，没有另一条 send-keys 支路。
- `runtime::journal`：每 attempt 独占 durable JSONL；不是第二个 outbox/store。
- `runtime::probes`：分层采集与组合门。OS、pane、MCP server、native binding、自然调用/响应/消费/呈现各自保留。

所有 provider 行为仍只有 K1 的七个 hooks。Clock、PhysicalTransport、AttemptJournal、BootstrapCommit 是共享框架端口，不是额外 provider hooks。

## 准备者 / K3 的调用约定

1. 框架分配 instance/generation、隔离 scope 与 private host directory；用 `resolve_cwd` 固定 cwd identity。`ScopedDirectory::create` 不收养已有目录；恢复只能使用自己的 durable `DirectoryReceipt`。
2. 通过 K1 resolver/H2 得到 plan，并完成 H3 materialization。`spawn_owned` 再核请求/plan、当前 candidate、executable hash、cwd、物化 receipt/hash 与文件路径。`LaunchPaths.candidate` 必须对应当前运行的新 candidate，不能指向旧 `team-agent`。
3. `SpawnReceipt` 的 target 只表示已绑定宿主/native 实例；不表示 composer 或 MCP Ready。部分 spawn 收据保留 owned files/possible spawn/returned pane/error，不以 Err 假装没有 effects。
4. K3 按真实 captured epoch/server instance 提供 `ProtocolEvent`。Configured、tools-list 响应写出不产生 ClientBound；无法证明 native client binding 时保持 Unknown。server instance 变更不能复用旧 binding/call 事件。
5. 用共享现有 `messaging::delivery::render_message` 渲染业务正文，传真实 durable message ID 和 renderer 已知的末尾 token range，构造 `PreparedEnvelope::from_rendered`。K2 不复制 renderer，也不从正文搜第一个/最后一个 token 作为权威 ID。
6. `resolve_submit_policy` 的输出现在保留 provider、operation、evidence kind、candidate hash。执行器须和实际接收端 TargetReceipt 全部匹配，不能拿发件方策略、另一操作的验证或 fixture 证据套在 native target 上。
7. 给每次调用新 attempt ID、绝对单调 deadline、FileJournal、当前 protocol snapshot 与可选 observer。普通发送由 executor 持锁；session-inspect/fork/stop 等多阶段操作可由 K3 先持有同一 directory-bound lane，再借给 executor。
8. 仅显式 FirstBusiness bootstrap 可使用 BootstrapCommit；其 scope/generation-fenced durable 消费必须在首次 native input 前成功。K2 不实现另一套消息/次数存储。T3c 不作为首条消息自锁条件。

## 字节与物理事件

- 业务 envelope 末尾是 trusted token，没有自动 LF。NativeControl 使用 profile 数据定义的有限 slash command/unsigned argument，没有 Team token。空业务正文与无 payload 的 StartupAck 不是一回事。
- 未经当前 framing 协议支持的 C0/DEL（含 ESC、NUL）在输入前拒绝，绝不悄悄清洗。LF/CR/TAB、引号和 Unicode 原样保留。用户正文不能嵌入 bracketed-paste 结束符逃出 payload。
- K2 的文本输入限定 Bracketed 模式。Tmux `paste-buffer -p` **只在应用请求 MODE_BRACKETPASTE 时加 framing**，且缺省会将 LF 换为 CR；这不是所需字节保证。因此新后端把完整成对 framing 放入同一个 owned buffer，用唯一一次 `paste-buffer -r` 保留 LF。没有恢复路径发送孤立 CSI201~，没有额外 Escape/Ctrl-C。
- 一个 attempt 最多一次 paste。首次 Enter、每级 confirmation、每次 retry、wrap-gap、queue flush、startup ack、host copy-mode cancel 分开计数；issued/confirmed/uncertain 不能互换。`1 + C + R + W + ΣQ` 和绝对 deadline 同时限界。
- 每个键前重新核 target/owner/generation/geometry、host mode、当前 frame/attempt/message guard。确认不存在不变 retry；Busy 曾被观察到后不再自动加键。单键已被 native 接受时不补“第二 Enter”。
- 本次 paste latch 是局部状态。NeverSeen、Seen、Gone 不互相伪装，不能换 fold identity 或由历史 token 授权新键。

## Journals / effects

Intent 在可能 paste/key 前 durable 写入；物理结果、H6 native acceptance 与 observer/audit 的失败分别记录。强事实先保留到内存收据，再尝试后续 audit/observer；失败不能降 effect floor，也不能自动重贴。

`FileJournal` exclusive-create 同一 attempt，存在即拒绝重放。恢复区分 Absent、NoInputIntent、MayHaveEffect、Complete 与 UnknownMayHaveEffect；torn/malformed/mismatched journal 不补造成功，后续损坏也保留已知 Submitted floor。恢复只分类，不执行任何输入。

Journal 记录 identity/hash/length/steps，不 dump prompt、env、payload 或 native captures。写入/hash 不确定的文件使用 K1 MayHaveWritten；native transcript/business/MCP 返回仍需 K3/K4 各自举证。

## 清场与限制

`close_owned_pane` 只关闭身份仍匹配的 owned pane并观察原 native 进程；不宣称整个进程树/Team 已 clean。私有 tmux server按 exit-empty 自然退出；代码不 kill-server、不删 socket、不碰默认 endpoint。文件清理要求生命周期排除 writer、同一 lane 与 compare-own-bytes；不是对任意外部 writer 的原子 CAS，dirty/uncertain 路径必须保留。K3 负责完整资源事务、残留与补偿。

文件访问限本地已授权的目录/常规文件；每次读取有 byte/application-time budget。标准 OS 同步文件调用不是对内核故障或非本地文件系统卡死的绝对实时保证。command 管道/等待则没有额外无界 join。Windows/ConPTY、ACP、直接 stdin 非交互不走 Tmux fallback。

## 验证入口（只在 Grok 或授权 CI）

准备者先核 Rust 1.95、tmux、Python 3，提供绝对路径（普通测试者不补环境）：

```sh
export CONTRACT_TMUX="$(command -v tmux)"
export CONTRACT_PYTHON="$(python3 -c 'import os,sys; print(os.path.realpath(sys.executable))')"
export CONTRACT_OLD_SCOPE=fixture-old-scope
export TEAM_AGENT_K2_FOREIGN_PROBE=fixture-foreign-team
cargo test --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets -- --nocapture
cargo clippy --manifest-path crates/team-agent-contract/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path crates/team-agent-contract/Cargo.toml --check
```

`physical_executor` 是 fake-clock/transport 的有限故障注入；`host_io` 检查管道/文件/journal；`probe_collection` 检查证据分层与 epoch；`host_native` 在隔离真实 tmux 内运行受控 fake TTY，验证实际 bytes、keypress、foreign buffer保留和 owned pane/native/private-server退出。后者输出 CONTROLLED_NATIVE_RECEIPT，明确 `fixture=not-kiro`，保留精确本用例文件供构建 owner 收取/归档。

Mac 本机禁止执行上述命令；新增独立 CI 只验证 macOS 受控 fixture，不代替本机 Luna/Kiro 订阅验收。实际 source/tree/lock、退出码、test counts、失败与清理/保留事实以本 PR 收据为准，本文不预宣称通过。
