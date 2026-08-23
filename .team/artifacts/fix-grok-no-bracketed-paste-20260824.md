# Grok 文本注入禁用 bracketed paste

- 基线：`origin/integration/0.5.x@399aa101ff205e66125ed8161196180e355d1e2d`
- 分支：`fix/grok-no-bracketed-paste-20260824`
- 产品提交：`35b1d97f90ce3501b75ee491f497d98745a2e2f0`
- 修改：Grok worker 与 leader fallback 文本注入传 `bracketed=false`；其他 provider 保持原行为。
- 未改提交键、`C-m`、`tmux_submit_key_name`，未发 Escape、CSI 201~ 或孤立 201~。

## 证据

- 基线装置：`.team/scripts/repro_grok_inject_newline_clipboard.sh 3`，退出码 `2`（两现象未同时命中），证据根
  `.team/artifacts/repro-grok-inject-runs/20260823T195721Z-72069/`；`trial-1..3/result.txt` 均为
  `image_hit=yes`，即图片首图命中 `3/3`。
- 改后同装置的非 bracketed 变体尝试 3 次，证据根
  `.team/artifacts/repro-grok-no-bracketed-runs/20260823T195953Z-98403/`；三次均在 40 秒内未见 Grok prompt，未取得图片判定，故该装置结果不可判。
- Grok 定向单测通过路径：`cargo test -p team-agent --lib paste_floor_tests` 经 `.claude/skills/grok-bot-tests/scripts/offload.sh` 发起，但远端起跑前因
  `/sys/fs/cgroup/gb-offload/grok-no-bracketed-test/memory.max: Permission denied` 失败，未产生 `ExecMainStatus`，不能冒充测试通过。
- 静态检查：`git diff --check` 通过；定向 argv 测试代码冻结 Grok paste argv 无 `-p`，Claude paste argv 仍有 `-p`。

verdict: unjudgeable
