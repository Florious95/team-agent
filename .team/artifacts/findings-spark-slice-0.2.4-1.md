# Findings — spark-reviewer (0.2.4-1)

- **HIGH** 5e0b14a — `src/team_agent/leader/__init__.py:729-737`
  - 在 `claim_leader` 的 ambiguous 候选分支，仍直接 `save_team_scoped_state(workspace, state)`，没有走 `_write_lease_dual_state()`。
  - 该分支会修改 `team_owner/leader_receiver` 但只写入主 runtime json，`team/<session>/state.json` 不同步更新，仍保留旧 lease 片段，和“3 个动词统一、双状态不分裂”的设计目标冲突。
  - 建议修复：将该分支统一到同一 lease 路径（读 `incident` 后复用 `_claim_lease_no_incident` 或抽成共同 helper）并在同一临界区内写 `state` 与 team runtime snapshot，必要时触发 `state_divergence_repaired`。

- **MEDIUM** 5e0b14a — `src/team_agent/leader/__init__.py:503` 与 `src/team_agent/leader/__init__.py:33-34`
  - `_try_readopt_leader_pane()` 在 attach-leader 回退路径中会调用 `_write_lease_dual_state()`，但 `attach_leader()` 并无 `runtime_lock`，仅在结束时 `save_runtime_state()`。
  - `_write_lease_dual_state()` 注释称“双状态同一锁内写入”，当前实现未对 `leader.attach`/`runtime.attach` 路径加锁，存在并发下主/分片状态交错更新的窗口（例如并发 send/claim 同时改 owner/receiver）。
  - 建议修复：要求 `attach_leader`/`autobind_leader_receiver_from_env`/启动入口在调用 `_attach_leader_to_state` 前统一持有 `leader_receiver` 或 send 统一锁，并将单独持久化收敛到同一写入点。

- **MEDIUM** 5e0b14a — `src/team_agent/leader/__init__.py:393-403` 与 `src/team_agent/messaging/leader_panes.py:498-506`
  - `_pane_is_live_leader()` 仍以 `pane_current_command`/`_leader_command_provider` 作为“活跃领袖”判断的主要依据，`command == node/nodejs/claude[.exe]` 即被视为可活跃领袖；注释却描述为“进程树/leader_session_uuid 缓存”证据。
  - 这会在工作区内出现非领袖 `node/claude` 进程时误判为 live owner，阻断 `--confirm` recover 路径，反过来若真实 leader 已切到不匹配前景命令且未带 `leader_session_uuid` 又会被误判为 dead。
  - 建议修复：把 command 识别收窄到 owner identity（provider + 进程树/会话环境）双条件，避免仅凭命令名决定 liveness。

- **MEDIUM** 5e0b14a — `src/team_agent/runtime.py:620` 与 `src/team_agent/leader/__init__.py:702`
  - `takeover` 使用 `send` 锁，`claim_leader` 使用 `leader_receiver` 锁；`attach_leader_to_state` 调用方又未加锁。
  - 三个 lease 变更入口对同一 `team_owner/leader_receiver` 并未共享同一锁命名约束，违反“收敛到同一 lease 变更语义（含原子语义）”的方向性目标，跨路径并发表面上仍可能出现重试-日志竞态。
  - 建议修复：定义单一 lease mutex（例如 `leader_ownership`）并让 takeover/claim/attach 全部走同一锁路径，至少保证状态变更与事件发射在同一命名互斥内。

## Severity counts

- CRITICAL: 0
- HIGH: 1
- MEDIUM: 3
- LOW: 0
