# doctor / diagnose 实机盲测用例书（S1–S6）

本用例书只提供公开终端操作和用户可观察判据。测试者使用 Leader 固定的最新 macOS `team-agent` binary、隔离 HOME/workspace、Luna caller/被测 Agent 和准备者提供的 Team fixture；不自行构建、不修改 fixture、不 claim/takeover、不重发凑绿。

## 通用记录与停止规则

每次调用记录：binary 绝对路径与 SHA256（由准备者提供）、workspace/Team、完整 argv、开始/结束时间、stdout、stderr、真实退出码、输出字节数/行数/最大行字节数。JSON 必须另存原文；human 输出逐行记录。

- accepted/queued/pending 不等于完成；按用例给出的观察窗口等待自然回程。
- 首个意外错误、需要步骤外补救、或输出无法按预期解析时立即停止；保留现场和原始收据，不换模型、不换目录、不重发。
- 结果只归类为 `PASS`、`PRODUCT-FAIL`、`TEST-SETUP-FAIL`、`NOT-RUN` 或 `UNKNOWN`。取证后由 owner 做 scoped cleanup，再以新隔离目录和新候选重跑同一用例。
- 未授权的修复、清理、host-wide orphan 扫描不属于这些用例。

## S1 — 安装自检 / 没有 Team

**前置**：新建空 workspace；HOME 和 PATH 满足准备者的干净环境条件；没有 Team 配置、runtime 或既有 `.team`。

**步骤**

1. 在该目录运行 `team-agent doctor --workspace <workspace>`。
2. 在同一目录运行 `team-agent doctor --workspace <workspace> --json`。
3. 对同一目录重复步骤 1–2，改用 `team-agent diagnose`。

**预期**

- 两个名字均明确报告无运行态 Team（runtime `status=not_present`），不虚构 `session_missing`。
- 在准备条件满足时四次调用均真实 rc0；JSON 的 `issues` 与 `suggested_repairs` 为空，human 为紧凑、无控制字符的 `doctor:` Triage。
- 空目录不被命令创建 `.team` 或其它写入物；两种拼写输出、rc、操作语义相同。

**停止/取证**：任何新建文件、伪造缺失 session、非零 rc 或 alias 分叉即停止并保留四份输出与目录快照。

## S2 — 健康真实 Team

**前置**：准备者提供 disposable Team 配置和 workspace；使用公开 `quick-start` 启动，并等待预定 ready / 真实自然回程；启动 accepted 不能作为 ready 证据。

**步骤**

1. 按准备者给出的公开 `quick-start` 命令启动 Team，等待 ready 窗口。
2. 运行 `team-agent doctor --workspace <workspace> --team <team> --json`，再运行 human 形式。
3. 在稳定现场分别运行 `diagnose` 的同参数 JSON/human 形式并保存对照。

**预期**

- 报告同时反映选定 Team 的安装/环境事实和运行态；无 issue 时真实 rc0、`ok=true`、`issues=[]`。
- doctor 与 diagnose 结果逐字节、rc 和选中 scope 相同；human 以 `doctor:` 开头且无长 JSON dump。
- 普通健康诊断不执行显式 gate/fix，不写 workspace，不新增 provider 执行。

**停止/取证**：ready 未达成归 `UNKNOWN` 或 `TEST-SETUP-FAIL`，不可把 accepted 当 PASS；出现 issue、alias 分叉或写入则 `PRODUCT-FAIL` 并停。

## S3 — 异常失联 + 状态证据

**前置**：准备者提供另一个隔离 workspace：持久运行态存在，但指定 Team 的 session/worker 已缺失；如有 socket conflict，fixture 同时保留两端证据。测试者不注入故障。

**步骤**

1. 在准备者给定的失联 fixture 上运行 `team-agent doctor --workspace <workspace> --team <team> --json`。
2. 不改变现场，运行同参数 `team-agent diagnose`。
3. 保存两份 JSON 和 stderr，等待规定观察窗口后结束。

**预期**

- 两个名字均真实 rc1、`ok=false`，指出具体失联故障和建议 repair；runtime 不是 `not_present`。
- 失联不会被当作普通无 Team，不自动 restart，不触碰其它 Team；相同现场两别名同判、同 scope、同 issues 集合。
- 若预置 socket conflict，报告保留冲突双方相关细节，不以一次泛化 timeout 替代证据。

**停止/取证**：看到自动 restart、跨 Team 污染、runtime 被折叠为 absent、或一方 rc0 即停止。

## S4 — 重启 / 多 Team 选择

**前置**：准备者已按顺序启动同宿主 Team A/B，并提供 A/B 的 workspace、selector 与 ready 窗口；测试者只操作 A。

**步骤**

1. 对 A 执行公开 `restart`，等待自然完成；不要并发操作 B。
2. 在稳定现场运行 `doctor --workspace <workspace> --team A --json` 和 human 形式。
3. 运行相同两种形式的 `doctor --team B`，再以 `diagnose` 重复 A/B JSON 对照。

**预期**

- A 的重启只改变 A；A/B 报告中的 selected Team 与证据 scope 正确。
- A 的独立问题不能出现在 B；B 的状态不能污染 A；两名字在相同选定 Team 上同判。
- 重启中的两个独立进程短暂观测不同是允许的；等待稳定后不得用不同集合伪造 alias 通过。

**停止/取证**：selector 不存在/歧义未以 rc1 拒绝、A/B 串扰、或重启未完成即被误判为健康时停止。

## S5 — 正常关闭后自检

**前置**：owned Team 已健康并可公开关闭；准备者说明是否保留 shutdown 日志。

**步骤**

1. 用公开 `shutdown` 关闭指定 Team，等待终态，不在终态前运行诊断。
2. 在同目录运行 `doctor --workspace <workspace> --team <team> --json`。
3. 用 `diagnose` 重复 JSON，并保存目录/进程残留检查结果。

**预期**

- 正常 terminal state 后 no active runtime 不生成 `session_missing`；runtime 为 `not_present` 或受支持的 stopped/archived 终态说明。
- 两名字 rc、`ok`、issues 和 scope 一致；正常停止本身不会被说成异常失联。
- 若 fixture 仍有真实坏 schema/其它残留，必须如实 rc1 并保留该 failure；不可把所有 shutdown 现场强行判 rc0。

**停止/取证**：正常关闭被报成 live/missing session，或诊断重启/修复了已关闭 Team 时停止。

## S6 — 显式门禁与修复兼容

**前置**：准备者提供可复原 disposable gate/fix fixture、允许的目标 scope、确认要求和一次性修复许可。破坏性操作使用两份等价 fixture，不能在同一已改变现场比较两个名字。

**步骤**

1. 在只读 fixture 上分别运行 `doctor --comms --json` 与 `diagnose --comms --json`；保存 `boundary`、`scope`、`checks`、`status` 和真实 rc。
2. 分别运行无 gate 的 `--fix`、未知 gate、缺 `--confirm` 的安全拒绝用例（两名字各用复原 fixture）。
3. 在许可的 schema fixture 上运行一次 `--fix-schema`（先使用 doctor，再在等价复原 fixture 使用 diagnose），保存 backup、before/after、changes、checks、boundary；随后只对目标 scope 复查。

**预期**

- 无 gate 的 fix、unknown gate、缺 confirm 均明确拒绝，不能 fall-through 成绿色默认诊断；两个名字 rc/输出/拒绝原因一致。
- comms JSON 保留 boundary/scope/checks/status，且真实 provider SDK 调用数为零；默认诊断不隐式执行 comms/orphan/fix。
- 许可修复只影响目标 fixture/scope，回执保留既有证据；修复后未解决 failure 仍进入统一 `issues` 并决定 rc。
- 破坏性动作不以同一被修改现场的字节差异判 alias 分叉。

**停止/取证**：安全门禁被绕过、修改越界、JSON 回执丢失、provider 被调用或修复后 failure 消失但 rc0 时立即停止。

## 清场与报告

每个用例完成取证后只清理本用例 owner 的 workspace、Team、进程和 socket；保留原始失败收据。报告逐项列 `PASS/PRODUCT-FAIL/TEST-SETUP-FAIL/NOT-RUN/UNKNOWN`、真实 rc、输出计数、时间窗口、现场路径和清理证明；不以进程仍存活、命令 accepted 或一次空 collect 冒称成功。
