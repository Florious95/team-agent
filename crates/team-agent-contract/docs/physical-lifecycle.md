# K2/K3 physical bridge

`orchestration::physical::PhysicalRuntime` 是共享 Host port，不是第八个 provider hook，不持有 adapter queue。它将 K3 的生命周期/outbox 接到 K2 唯一物理 executor；所有 provider 行为仍来自原有 15 facets、7 hooks 与声明式 profile/control data。

## 已接线边界

- F0 保持纯 admission；F3 才创建实例私有目录并调用 `TmuxHost::spawn_owned`。参数/环境/配置由已校验 plan 和 journaled materialization receipt 提供。
- spawn 不再假设预分配 pane/binding 就是实际值：`SpawnedProcess` 返回实际 endpoint/pane/binding/process 及完整 `TargetReceipt`，K3 校验 owner/native/cwd/route 后在同一 F3 持久化，随后才能捕获 session/提交 F4。
- 完整目标收据随 SeatRecord 落在**同一个 ContractStore**。`TmuxHost::restore_owned` 只接受 supervisor 保存的收据，重新检查目录 dev/inode/uid/mode、owner→binding hash、socket dev/inode、pane、几何、PID birth/image 与 cwd。deserialize 成功不等于获得 OS authority。
- pane ID 只在 endpoint 内唯一；store 在 F0 与实际 F3 publish 均检查 endpoint+pane 或 binding 冲突，不能把不同私有 tmux endpoint 上的 `%0` 当同一 pane。
- H4 接收同一 native input lane 下的当前 pane 字节与 scope/hash；没有 SID 时不扫数据库/最近文件。成功 binding 的 SID 同步写入物理 receipt；后续 reopen 不恢复旧 session 的 probes。
- DeliveryHost 使用 shared Team envelope grammar（含可选 task header，typed ID 对应最后 token），相同 `resolve_submit_policy`、`deliver_envelope`、owned FileJournal、native lane。真实 transport 在输入边界重采样 T1/T2；缺少当前 protocol/profile evidence 就拒绝，不从已落地配置合成 T3。
- native controls 也调用 `inject_with_contract`；只有注册的 profile/hash/operation/control kind 编码允许执行，无直接 `send-keys` fallback。
- stop 只关闭已证的 owned pane 并等待捕获的 native process 退出；不 `kill-server`，不 unlink socket，不递归清理。只有本上下文确认 quiescence 后，才允许 descriptor-granted owned receipt 的精确清场。

## Tmux 元数据失败诊断

唯一 `TmuxHost::command` 入口固定使用 `tmux -u -S <owned socket>`：`-u` 明确选择 UTF-8 机器协议，不依赖 caller 的 `LANG`/`LC_*`，也不修改 native 子进程的 locale。tmux 3.7c 在缺少 UTF-8 client 标记时，会把 TAB 经 `utf8_sanitize` 转成 `_`；不能把 `_` 当作兼容分隔符，因为合法名称也可含下划线。

源码依据（官方 tag 3.7c，commit `e476c1230b958df0cb12977517d24b3dc931375b`）：[client UTF-8 选择](https://github.com/tmux/tmux/blob/e476c1230b958df0cb12977517d24b3dc931375b/tmux.c#L460-L503) → [输出卫生化](https://github.com/tmux/tmux/blob/e476c1230b958df0cb12977517d24b3dc931375b/server-client.c#L2828-L2840) → [控制字符替换](https://github.com/tmux/tmux/blob/e476c1230b958df0cb12977517d24b3dc931375b/utf8.c#L784-L814)。这是固定的因果证据引用，不是 tmux 版本或二进制 admission 门禁。

固定 `FORMAT` 的解析错误区分 `new-session` 首次返回与后续 `query`；空输出、字段数不等于 13（保留空字段并输出实际数量）、session/window/pane ID 非法分别报错。其他 PID、状态和尺寸校验仍严格拒绝，不因诊断而放宽身份围栏。

错误只附本次元数据响应的前 256 原始字节，按字节 ASCII 转义（最多 1024 字符），并标注总字节数及是否截断；不读取/打印 pane 屏幕、prompt、argv、环境、stderr 或认证配置。桥接保留这些详情到 lifecycle 的 `operation.failure`，而非只剩静态 `Host("tmux metadata shape")`。诊断不重试 spawn、不接管旧 pane、不清除 pending/lease。

## 原生控制输入与失败诊断

`ControlDefinition.input_mode` 显式区分沿用 profile paste 与 `DirectTyping`；只有 sealed native control 能选择后者，绝不根据业务正文的 `/` 前缀猜控制流。Kiro 的 `/session-id`、`/tools` 声明 DirectTyping，对齐 R0 的 `send-keys -l`；业务消息继续使用原有 bracketed paste。输入仍受同一 lane、owner/native fence、journal、H6 当前输入守卫和单 Enter 预算约束，无 adapter 私自调用 tmux 的旁路。

DirectTyping 复用本 attempt 的 owned buffer：暂存无 bracketed framing 的控制文本，定向读回并拒绝非可打印 ASCII/越界内容，重核 target 后以一次 `send-keys -l -t <owned pane> -- <text>` 插入；提交仍是下一次独立的单 Enter。既有 Paste 计数表示一次正文插入（此模式不是终端 paste 事件），`initial` 才计提交键；buffer 按原有作用域清理，foreign buffer 不动。

Kiro policy r2 hash 为 `721a29c11d8e23bdbd109056174ee033c5c8fac8e7cabd92e82eee849ae617e7`，SHA256 输入为 `kiro-v2-tui-macos:r2;business=bracketed;session-id=direct-typing;tools=direct-typing;enter=1;retry=never;trailer=none;capture-ms=100;stable-ms=200;paste-floor-ms=100;deadline-ms=30000`。此 profile recipe 变化不能沿用旧 attempt/protocol evidence。

`/session-id` 等控制仍通过唯一物理 executor，失败不重贴、不补回车。`Failure.code` 保存实际 `ExecutionProblem.code`，不再写无差别的 `execution-stopped`；若 journal 本身失败，缺失收据仍保持未知，不能补造。`physical::control` 的公开错误保留 operation、disposition、persistence、effect floor、全部物理计数和各 problem 的 code/HostError，不再统一折叠成 `native control outcome uncertain`。生命周期继续将错误的 Display 文本持久化为 `operation.failure`。

计数区分 paste、initial、confirmation、retry、wrap-gap、queue、startup、host-mode 和 capture；命令 Confirmed 不等于 NativeAccepted，MayHaveSubmitted 不得降成 NoEffect。诊断不含输入正文、终端屏幕、argv/env 或认证配置，不改变原来的成功判据、deadline、重试和补偿行为。

## Bootstrap 原子边界

Store schema **2** 在原库增加 `contract_bootstrap`（owner、唯一 attempt、confirmed），不是队列。旧未发布 schema 1 runtime root 不自动迁移/接管；重新开隔离 root，历史证据保留。

K3 claim 中的目标 lease、outbox in-flight、bootstrap-used 和 FirstBusiness reservation 在同一 SQL transaction。K2 `BootstrapCommit` 对接 `OutboxBootstrap`，只在当前 scope/instance/generation、同一 outbox attempt、同一 active lease 下确认一次；事务先于物理 paste。错 attempt、第二次确认、后续普通消息、过期 generation 都不能再使用这份额度。

`OutboxBootstrap` 持有同库另一条连接，避免 scheduler 在 callback 期间借用同一连接。bootstrap 不可重置；未知效果依旧隔离，无重贴或额外 provider queue。

## 明确限制

- 这是 library 组合端口，不是已经交付的 public coordinator/MCP subprocess CLI。实际 native MCP 父子进程绑定、实时 protocol producer、持久 supervisor 公共入口和 macOS candidate 仍未闭环。
- `PhysicalRuntime::protocol` / `evidence` 的输入由框架准备者提供；过期、不同 scope/profile/candidate 的值被底层拒绝。不能把 fixture evidence 写成 Native，不能给未知绑定拼 Observed。
- 控制执行可能发生但回程丢失时，K3 保留 pending fence；没有自动恢复/replay。spawn 部分失败时不得猜 PID、重新 spawn 或删除可能仍被使用的资源。
- 当前 session read 是 bounded pane snapshot；提供真实 session profile 前，不能宣称 `/session-id` inspection、resume 或 branch 已在 Kiro 生效。
- pre-spawn failure 没有 quiescence grant 时，owned materialized 资源保留；成功 stop 后仅删除已授权配置，host 脚本/attempt journals/diagnostics 保留作为证据。

## 受控验证

`tests/physical_lifecycle.rs` 覆盖动态路由持久化、endpoint-local pane、旧 binding 拒绝、实际路由冲突、同库 bootstrap 一次性确认、shared renderer 以及真实 tmux+Python 的 startup→reopen→fence→shared outbox delivery→teardown。Python 不包含 native Kiro/MCP client；T3 输入明确标为 Fixture，不能据此宣传 Kiro 端到端通过。测试 root/退出/保留日志写入受控 receipt，构建者负责归档和 scoped 清理。

`tests/host_native.rs` 的真实 tmux+Python 用例逐请求移除 `LC_ALL`/`LC_CTYPE`/`LANG`，覆盖无 locale 下 startup/query/Unicode capture/delivery/close；新增 DirectTyping 用例核原始 typed bytes、零 bracketed paste、恰好一枚 Enter 和 owned buffer 清理；不修改并行测试进程的全局环境。`tests/host_io.rs` 保留 R4 下划线损坏帧的拒绝回归，确保修复发生在 tmux 客户端输出边界，而不是猜测式放宽解析。

所有 Cargo/fmt/clippy 仅在 Grok/正式授权 CI；新增用例在取得具体运行收据前保持 NOT-RUN。
