#!/bin/sh
# //!
# //! purpose: busy-shell 不重复齿——pty 被占时 Unverified 补发路径不得再按一颗 C-m
# //! contract:
# //!   provides:
# //!     - name: stall-busy-nodup
# //!       what: holdMs 内不读 stdin；send 后恢复；side 按 CR 计行；不得比 inject 基线多一颗（send 路径基线 5，不是 raw 单行的 1）
# //!   requires:
# //!     - name: team-agent-gauge
# //!       what: TEAM_AGENT_BIN 指向拷出件
# //! boundary:
# //!   - 三态 0/1/2；final_lines>EXPECT_LINES 为补发重复红；0 为未到达；=EXPECT_LINES 为不重复
# //!   - 隔离 ws+socket；不继承 raw TMUX/TMUX_PANE；不重粘、不用 Escape/C-c
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记下的 PID
# //! maturity: wired
#
# 用法: TEAM_AGENT_BIN=/path/to/copied-bin sh scripts/repro/repro_stall_busy_nodup.sh

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
NODE_BIN="${NODE_BIN:-/opt/homebrew/bin/node}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/Volumes/nvme/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-d126-busy-$$"
TEAM_ID="d126b$$"
AGENT_ID="w1"
WS="$RUN/ws"
TEAMDIR="$WS/team"
BINDIR="$RUN/bin"
HOLD_MS="${HOLD_MS:-12000}"
WAIT_AFTER="${WAIT_AFTER:-20}"
# send 路径 157ae109 实测 inject 对 busy-hold 落 5 颗 C-m；补发闸不得变成 6。
EXPECT_LINES="${EXPECT_LINES:-5}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
PIDS=""
SOCK=""
STARTED=$(date +%s)
CLEANED=0
TA=""
BIN_MD5=""
TMUX_BIN=""

unjudgeable() { echo "UNJUDGEABLE: $*" >&2; finish 2; }
fail_repro() { echo "DUP: $*" >&2; finish 1; }
pass_ok() { echo "NODUP: $*" >&2; finish 0; }

record_pid() {
  _p=$1
  [ -n "$_p" ] || return 0
  case " $PIDS " in
    *" $_p "*) return 0 ;;
  esac
  PIDS="$PIDS $_p"
}

narrow_ps() { ps -o pid,ppid,etime,stat,comm -p "$1" 2>/dev/null || true; }
pid_alive() {
  _out=$(narrow_ps "$1")
  echo "$_out" | awk -v p="$1" 'NR>1 && $1==p {found=1} END {exit found?0:1}'
}
protected_name() {
  case "$PROTECTED" in
    *" $1 "*) return 0 ;;
  esac
  return 1
}
say() { printf '%s\n' "$*"; }

finish() {
  _rc=$1
  say "RESULT rc=$_rc bin=$TA md5=$BIN_MD5"
  if [ "$CLEANED" -eq 0 ]; then
    CLEANED=1
    if [ -n "$TA" ] && [ -n "$WS" ] && [ -x "$TA" ]; then
      "$TA" shutdown --workspace "$WS" --team "$TEAM_ID" --keep-logs --json >/dev/null 2>&1 || true
    fi
    for _p in $PIDS; do
      if pid_alive "$_p"; then
        kill -TERM "$_p" 2>/dev/null || true
      fi
    done
    sleep 1
    for _p in $PIDS; do
      if pid_alive "$_p"; then
        kill -KILL "$_p" 2>/dev/null || true
      fi
    done
    if [ -n "$SOCK" ] && [ -n "$TMUX_BIN" ]; then
      if ! protected_name "$SOCK"; then
        "$TMUX_BIN" -L "$SOCK" kill-server >/dev/null 2>&1 || true
      fi
    fi
    if [ "${KEEP_TMP:-0}" != "1" ] && [ -n "$RUN" ]; then
      case "$RUN" in
        *ta-d126-busy-*) rm -rf "$RUN" ;;
      esac
    fi
  fi
  exit "$_rc"
}

trap 'unjudgeable interrupted' INT TERM
unset TMUX TMUX_PANE
_oldifs=$IFS
IFS='
'
for _line in $(env); do
  _k=${_line%%=*}
  case "$_k" in
    TEAM_AGENT_*) unset "$_k" ;;
  esac
done
IFS=$_oldifs

[ -x "$SRC_BIN" ] || unjudgeable "gauge missing: $SRC_BIN"
if [ ! -x "$REAL_TMUX" ]; then
  command -v tmux >/dev/null 2>&1 || unjudgeable "tmux not found"
  REAL_TMUX=$(command -v tmux)
fi
[ -x "$NODE_BIN" ] || unjudgeable "node missing"
TMUX_BIN="$REAL_TMUX"

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$BINDIR" || unjudgeable "mkdir failed"
TA="$BINDIR/team-agent"
cp "$SRC_BIN" "$TA" || unjudgeable "cp gauge failed"
chmod +x "$TA"
BIN_MD5=$(md5 -q "$TA" 2>/dev/null || md5sum "$TA" | awk '{print $1}')
BIN_MTIME=$(stat -f '%Sm' "$TA" 2>/dev/null || stat -c '%y' "$TA" 2>/dev/null || echo unknown)
say "GAUGE path=$TA md5=$BIN_MD5 mtime=$BIN_MTIME version=$("$TA" --version 2>/dev/null || true)"
cp "$NODE/busy-hold.js" "$RUN/busy-hold.js" || unjudgeable "copy busy-hold.js failed"
SIDE="$RUN/busy.side"

cat > "$TEAMDIR/TEAM.md" <<EOF
---
name: $TEAM_ID
objective: stall-busy-nodup
provider: fake
display_backend: none
---

Probe.
EOF
cat > "$TEAMDIR/agents/${AGENT_ID}.md" <<EOF
---
name: $AGENT_ID
role: Worker
provider: fake
model: fake
tools:
  - mcp_team
dangerously_skip_permissions: false
---

Wait.
EOF

"$TA" quick-start "$TEAMDIR" --workspace "$WS" --yes --no-display --json >"$RUN/qs.json" 2>"$RUN/qs.err" || true
python3 - "$RUN/qs.json" <<'PY' || unjudgeable "quick-start did not spawn workers"
import json, sys
d = json.load(open(sys.argv[1]))
wr = d.get("worker_readiness") or d.get("readiness") or {}
if wr.get("all_workers_spawned") is not True:
    raise SystemExit(1)
print("all_workers_spawned true")
PY

if [ -f "$WS/.team/runtime/coordinator.pid" ]; then
  record_pid "$(tr -d ' \n' < "$WS/.team/runtime/coordinator.pid")"
fi
SOCK=$(python3 - "$WS/.team/runtime/state.json" <<'PY'
import json, sys, os
d = json.load(open(sys.argv[1]))
print(os.path.basename(d.get("tmux_socket") or ""))
PY
)
[ -n "$SOCK" ] || unjudgeable "empty tmux_socket"
protected_name "$SOCK" && unjudgeable "protected socket"
say "SOCK=$SOCK"

t=0
W1=""
while [ "$t" -lt 30 ]; do
  W1=$("$TMUX_BIN" -L "$SOCK" list-windows -a -F '#{window_name} #{pane_id}' 2>/dev/null | awk -v a="$AGENT_ID" '$1==a{print $2; exit}')
  [ -n "$W1" ] && break
  sleep 1
  t=$((t + 1))
done
[ -n "$W1" ] || unjudgeable "w1 pane not found"

"$TMUX_BIN" -L "$SOCK" respawn-pane -k -t "$W1" -- "$NODE_BIN" "$RUN/busy-hold.js" "$SIDE" "$HOLD_MS"
sleep 1
record_pid "$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1" '#{pane_pid}' 2>/dev/null || true)"
t=0
while [ "$t" -lt 10 ]; do
  cap=$("$TMUX_BIN" -L "$SOCK" capture-pane -p -t "$W1" 2>/dev/null || true)
  case "$cap" in
    *busy-hold-ready*) break ;;
  esac
  sleep 1
  t=$((t + 1))
done
case "$cap" in
  *busy-hold-ready*) say "busy-hold ready" ;;
  *) unjudgeable "busy-hold never ready" ;;
esac

PAYLOAD="BUSYNODUP-$$"
"$TA" send "$AGENT_ID" "$PAYLOAD" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send.json" 2>"$RUN/send.err" || unjudgeable "send failed"
say "SEND $(cat "$RUN/send.json" 2>/dev/null | tr '\n' ' ')"

# wait hold + drain
sleep $((HOLD_MS / 1000 + WAIT_AFTER))
FINAL=0
[ -f "$SIDE" ] && FINAL=$(awk 'END {print NR+0}' "$SIDE")
say "final_lines=$FINAL side=$SIDE"
if [ "$FINAL" -eq 0 ]; then
  unjudgeable "side empty — inject did not reach the busy pane (anti-vacuous fail)"
fi
if [ "$FINAL" -eq "$EXPECT_LINES" ]; then
  pass_ok "final_lines=$FINAL expect=$EXPECT_LINES (no extra Unverified resend)"
fi
if [ "$FINAL" -gt "$EXPECT_LINES" ]; then
  fail_repro "final_lines=$FINAL > expect=$EXPECT_LINES (Unverified resend duplicated)"
fi
unjudgeable "final_lines=$FINAL < expect=$EXPECT_LINES (inject did not match send-path baseline)"
