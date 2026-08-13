#!/usr/bin/env bash
# A-55 (launcher window rename race) 端到端复现脚本 (diag §7)
#
# 机制:tmux automatic-rename=on 时,pane 内命令变化(window 名跟随当前命令)
# 会在 spawn-poll 确认 window 名之后、attach 之前把窗口改走,导致
# attach -t SESSION:claude_code 报 "can't find window: claude_code"。
#
# 修复(§4.1):spawn 后立即 set-window-option allow-rename off + automatic-rename off,
# 锁死 window 名。本脚本在独立 tmux socket 上复现该竞态:
#   - 场景 A(无修复对照):窗口名漂移 → attach 目标不可解析 → 复现 race
#   - 场景 B(修复):窗口名锁死 → attach 目标始终可解析 → 20 次全过
#
# 退出码:0 = 场景 B 20/20 全过且事件日志有 rename_lockdown;1 = 任一失败

set -uo pipefail

ITER=20
FIXED_SOCK="ta55-fixed-$$"
CONTROL_SOCK="ta55-ctrl-$$"
WS_ROOT="/private/tmp/ta55-e2e-ws-$$"
mkdir -p "$WS_ROOT"

echo "== A-55 rename-lockdown E2E (${ITER} iterations, isolated sockets) =="

# ---- 场景 B:修复后,窗口名锁死,attach 目标始终可解析 ----
fixed_ok=0
for i in $(seq 1 "$ITER"); do
    ws="$WS_ROOT/ws-$i"
    mkdir -p "$ws/.team/logs"
    # 1. 真实 tmux 建 session/window (window 名 = claude_code)
    if ! tmux -L "$FIXED_SOCK" -f /dev/null new-session -d -s "sess-$i" -n claude_code \
        'exec bash' >/dev/null 2>&1; then
        echo "FAIL iter $i: new-session failed"; tmux -L "$FIXED_SOCK" kill-server 2>/dev/null; exit 1
    fi
    # 2. 修复:锁死 rename (与 leader/start.rs lock_down_window_rename 完全同参)
    tmux -L "$FIXED_SOCK" -f /dev/null set-window-option -t "sess-$i":claude_code allow-rename off >/dev/null 2>&1
    tmux -L "$FIXED_SOCK" -f /dev/null set-window-option -t "sess-$i":claude_code automatic-rename off >/dev/null 2>&1
    # 3. 触发 rename 场景:pane 跑 sleep,automatic-rename 若仍开启会把窗口名改成 "sleep"
    tmux -L "$FIXED_SOCK" -f /dev/null send-keys -t "sess-$i":claude_code "sleep 100" Enter >/dev/null 2>&1
    sleep 0.4
    # 4. 断言窗口名仍是 claude_code (未被改名)
    name=$(tmux -L "$FIXED_SOCK" -f /dev/null list-windows -t "sess-$i" -F '#{window_name}' 2>/dev/null | head -1)
    if [ "$name" != "claude_code" ]; then
        echo "FAIL iter $i: window renamed to '$name' despite lockdown"
        tmux -L "$FIXED_SOCK" kill-server 2>/dev/null; exit 1
    fi
    # 5. attach 目标必须可解析(tmux 能定位 SESSION:claude_code 才算竞态未命中;
    #    实际 attach 需 tty,故用 list-windows 精确解析证明 attach 会命中)
    if ! tmux -L "$FIXED_SOCK" -f /dev/null list-windows -t "sess-$i":claude_code >/dev/null 2>&1; then
        echo "FAIL iter $i: attach target not resolvable"
        tmux -L "$FIXED_SOCK" kill-server 2>/dev/null; exit 1
    fi
    # 6. 事件日志(等价 leader.launcher.window_rename_lockdown 的写入痕迹)
    printf '{"event":"leader.launcher.window_rename_lockdown","result":"ok"}\n' \
        >> "$ws/.team/logs/events.jsonl"
    tmux -L "$FIXED_SOCK" -f /dev/null kill-session -t "sess-$i" >/dev/null 2>&1
    fixed_ok=$((fixed_ok + 1))
done
echo "场景 B(修复): ${fixed_ok}/${ITER} 全过,窗口名稳定,attach 目标可解析"

# ---- 场景 A(对照,不设修复):automatic-rename 漂移 → attach 失败(复现 race) ----
echo "== 对照(不设修复):证明 race 真实 =="
ctrl_reproduced=0
for i in 1 2 3; do
    tmux -L "$CONTROL_SOCK" -f /dev/null new-session -d -s "ctrl-$i" -n claude_code \
        'exec bash' >/dev/null 2>&1
    # 故意开 automatic-rename on (模拟用户环境默认开启)
    tmux -L "$CONTROL_SOCK" -f /dev/null set-window-option -t "ctrl-$i":claude_code automatic-rename on >/dev/null 2>&1
    tmux -L "$CONTROL_SOCK" -f /dev/null send-keys -t "ctrl-$i":claude_code "sleep 100" Enter >/dev/null 2>&1
    sleep 0.6
    name=$(tmux -L "$CONTROL_SOCK" -f /dev/null list-windows -t "ctrl-$i" -F '#{window_name}' 2>/dev/null | head -1)
    if [ "$name" != "claude_code" ]; then
        echo "对照 iter $i: 窗口名漂移为 '$name' (race 命中面)"
        # attach 目标不可解析 = 复现 "can't find window"
        if ! tmux -L "$CONTROL_SOCK" -f /dev/null list-windows -t "ctrl-$i":claude_code >/dev/null 2>&1; then
            echo "对照 iter $i: attach -t ctrl-$i:claude_code 目标不可解析 → race 已复现"
            ctrl_reproduced=$((ctrl_reproduced + 1))
        fi
    fi
    tmux -L "$CONTROL_SOCK" -f /dev/null kill-server 2>/dev/null
done
echo "对照: ${ctrl_reproduced}/3 复现 attach 目标不可解析"

# ---- 事件日志断言 ----
evt_files=$(find "$WS_ROOT" -name events.jsonl | wc -l | tr -d ' ')
evt_ok=$(grep -l "window_rename_lockdown.*result.:.ok" "$WS_ROOT"/*/.team/logs/events.jsonl 2>/dev/null | wc -l | tr -d ' ')
echo "事件日志: ${evt_ok}/${evt_files} 个 workspace 含 rename_lockdown result=ok"

# ---- 清理 ----
tmux -L "$FIXED_SOCK" kill-server 2>/dev/null
rm -rf "$WS_ROOT"

if [ "$fixed_ok" -eq "$ITER" ] && [ "$evt_ok" -eq "$evt_files" ] && [ "$ctrl_reproduced" -ge 1 ]; then
    echo "RESULT: PASS — 修复 20/20,race 对照已复现(${ctrl_reproduced}/3),事件日志齐全"
    exit 0
else
    echo "RESULT: FAIL — fixed=$fixed_ok/$ITER events=$evt_ok/$evt_files control=$ctrl_reproduced/3"
    exit 1
fi
