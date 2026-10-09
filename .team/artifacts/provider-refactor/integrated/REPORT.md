# Kiro contract integrated：实施与证据报告

**阶段：集成代码与受控验证；不是 Kiro 全功能/K5 完成。**

Review：<https://github.com/Florious95/team-agent/pull/307>（按 leader 指令转 Ready for Review；不是 native Ready）

Branch：`feat/kiro-contract-integrated`

产品代码冻结：`12ef1aafb1cd2031894d73c6511428bc72b4c1a2`，tree `c4bb341029747366ecd5d50ae039c00b7760c2f9`。

最终测试 head：**`be67a0128dc06e6692e095d92bb19252c642145e`**，tree **`5015793824f894594c169ec13cd21e3c1f02f3fc`**。91533932 + be67a012 仅为测试 helper 简化/格式化；src/Cargo.toml/Cargo.lock 对产品冻结 diff=0。完整最终 checks 均 exit0。随后本报告/日志归档为 evidence-only，不改变 package bytes。

## 解决的问题与设计取舍

1. **组合而不迁移六家。** K1、K2、K3、K4 进入独立 nested package，同一 scope/store；旧 provider、根 manifest/lockfile、AGENTS/CLAUDE 对 K1 冻结 `2e43ad02` diff=0。没有 merge/main/tag/release。
2. **工作区配置不是任意写权限。** descriptor scope + seat 捕获 resource-kind grant + 原 cwd identity 驱动 JournaledIo；真实 ScopedMaterializer no-follow/独占/不 chmod 现有目录，file creation identity + hash 支撑有限清场。禁止全局配置/原生账号数据库。
3. **真正执行引擎而非包装器。** 观察到 wrapper 硬编码失败且 PATH 无效后，解析 local/bundle `kiro-cli-chat`，验证 regular executable、实际版本与 fingerprint；PlanHook 不接受 wrapper。
4. **失败物证不能美化成成功目录。** 101586-byte output 是登录 spinner，不是 JSON。公共 CommandRunner 有限输出/deadline/closed stdin，观察 auth marker 则取消自有 child；Kiro read host 返回 AuthRequired，不登录、不换模型、不重试、不伪造 catalog。
5. **实际路由才能持久化。** F3 保存真实 endpoint/pane/binding/TargetReceipt，而不是预分配占位；恢复核 owner→binding、socket inode、cwd、PID birth/image。pane ID 只在 endpoint 内唯一。没有发现/接管默认 tmux server。
6. **只有一个共享执行链。** PhysicalRuntime 接 K3 生命周期/同库 outbox 与 K2 executor/journal。业务和 native control 共用物理入口；同库 BootstrapCommit 一次确认、错误 scope/generation/attempt/lease 拒绝，无 adapter queue/replay。
7. **native slot 缺证时拒绝。** 五档 effort、model/list format 的语法有 native help；输入提示符、Enter timing、SID、MCP binding、branch 没有当前原始帧，不填虚假的生产默认值。H1/H4/H5/H6/H7 保持未验证，FullSnapshot/NativeNewSeat 明确 Unsupported。

实现导航：package `docs/kiro-adapter.md`、`docs/physical-lifecycle.md`、`docs/physical-executor.md`、`docs/lifecycle-mcp.md`。

## Grok 自动化历史（失败保留）

以下同名子目录保存原始 receipt。复制前在原目录对 `artifact-sha256` 执行校验全部 OK；`stdout.log` 用于阅读，权威 journal/status/command/exit 等由原 manifest 绑定。count 为真实 `test result` 汇总，零项 lib target 不计作一个用例。

| Unit / source | 实际用例 | Test | Clippy | 结论 |
|---|---:|---:|---:|---|
| `gb-09160774-contract-verify-a1` | 134P / 0F / 0I | 0 | 101 | 4 处样式错误，已修 |
| `gb-4ffdf965-contract-validation-a1` | 70P / 1F / 0I（后续未执行） | 101 | NOT-RUN | 测试 shell octal escape 被 Rust 解成 NUL；runner 正确 NotStarted。改 raw string，不改判据 |
| `gb-8a53aa83-contract-final-a1` | 147P / 0F / 0I | 0 | 101 | 2 处 nonminimal_bool，已修 |
| `gb-cc508177-contract-final-a1` | **未编译完成 / 未运行** | 101 | NOT-RUN | Bootstrap mutable trait-object lifetime 与 attempt locals 错误耦合；独立生命周期参数修复，无 static/leak/unsafe |
| `gb-12ef1aaf-contract-validation-a1` | **152P / 0F / 0I** | **0** | 101 | 产品/新增组合用例通过；2处测试 filter-next 已按 rfind 等价修复 |
| **`gb-be67a012-contract-final-a1`** | **152P / 0F / 0I** | **0** | **0** | **fmt --check=0；CommandExit/ExecMainStatus=0；最终受控门 PASS** |

152 项包括：34 contracts、14 host I/O、4 real-tmux fake-process、13 Kiro argv/materialize、6 bounded catalog、6 helper dispatch、16 lifecycle/MCP、27 physical executor、5 physical lifecycle bridge、9 probe、8 resource grants、10 fault transactions。不是订阅 native Kiro 测试。

## 原生事实与不可越过的门

Runner 原始路径：`/Volumes/nvme/tmp/kiro-native-receipts/`，子命令 `chat-subcommands/REPORT.md`。

- direct helper version2.28.0/root help/chat help exit0，helper SHA256 `430aae1a4f5ae252e785114d45d61ec465796ad6ca96383fb7ca383dd96b1712`。
- chat help SHA256 `47e22e67d2d6042ceff70b5d6414ece1a367bbbbd675c873a39d1dd3e868bf20`；支持 model/effort/list-only format。
- catalog partial stdout SHA256 `7abd2c71c8eeef941f7ae0eb733f358c880f1ab19fcb73fe568d17b566d25599`，JSON byte0 即失败，重复 auth portal spinner。外层180s timeout，真实 native exit unknown。
- 认证由用户处理；取消 child 不保证撤销它此前打开的浏览器。不读取 credential store，不自动 login，不换 Claude 绕过 Luna 限制。
- 没有 authenticated catalog/schema/native Luna ID，没有 native terminal/session/MCP/Fork acceptance；K5 **NOT-RUN**。没有 macOS public runtime candidate。

## 仍未完成 / 资源

- Public supervisor/MCP subprocess 入口与 native connection provenance 尚未落地；library 组合端口不能冒充可用的普通用户 CLI。
- Native H4/H5/H6/H7/profile 与 H1 catalog 缺证；H2 的 Native admission barrier 保留，不能复制 fixture descriptor 来启用。
- Final validation unit `gb-be67a012-contract-final-a1` 与 fmt-prep unit `gb-91533932-contract-fmt-prep-a1`：builder result `res_cdd64c1d45bc` 报 Wiped=yes；本席随后仅用官方 `status.sh --no-install --unit <exact>` 只读回核，均为 **terminal / success / Wiped=yes / MainPID空 / Process空**。两个 `post-wipe-status.txt` 已归档，local-cleanup 另存；没有重新安装 helper、重跑测试或执行清理。
- 历史 status 是 fetch 时的 wipe 前快照，保持原文；不篡改成 Wiped=yes。历史5次的清场不由最终两个 unit 的证明外推。
- 五个最终 controlled tmux fixture 的 native/server 退出由成功测试及日志覆盖；测试代码声明保留 `/tmp/tac-k2-*`、`/tmp/tac-k23-*` 的 owned journals/diagnostics 供 PR 审阅。unit wipe 不等于这些日志目录删除，本席未另做目录删除/复核，不宣传“所有临时文件均删除”。最终具体路径列于 final stdout/journal。后续精确清理由 builder/leader 按 receipt 处理。
- PR301/305/306 历史入口未擅自关闭/合并；PR307 只转审阅。此报告不授权重新跑原生认证命令或发布。
