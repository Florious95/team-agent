#!/bin/sh
# //!
# //! purpose: 非 copy-mode Unverified 滞留——TUI composer 仍有文本时框架须补一颗 C-m
# //! contract:
# //!   provides:
# //!     - name: stall-resend-noncopy
# //!       what: drop-enter 先丢 1 颗 Enter；修前 exit 1（90s 内无提交）；修后 exit 0（补发后提交）
# //!   requires:
# //!     - name: team-agent-gauge
# //!       what: TEAM_AGENT_BIN 指向拷出件；禁止 exec 正在被 cargo 写的 target
# //! boundary:
# //!   - 三态 0/1/2；禁止把 2 折进 0/1
# //!   - 隔离 ws+socket；不继承 raw TMUX/TMUX_PANE
# //!   - 不重粘、不用 Escape/C-c；只杀本脚本记下的 PID
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm
# //! maturity: wired
#
# 用法: TEAM_AGENT_BIN=/path/to/copied-bin sh scripts/repro/repro_stall_resend_noncopy.sh

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
NODE_BIN="${NODE_BIN:-/opt/homebrew/bin/node}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/Volumes/nvme/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-d126-stall-$$"
TEAM_ID="d126s$$"
AGENT_ID="w1"
WS="$RUN/ws"
TEAMDIR="$WS/team"
BINDIR="$RUN/bin"
BUDGET_SEC="${BUDGET_SEC:-120}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
PIDS=""
SOCK=""
STARTED=$(date +%s)
CLEANED=0
TA=""
BIN_MD5=""
TMUX_BIN=""

unjudgeable() { echo "UNJUDGEABLE: $*" >&2; finish 2; }
fail_repro() { echo "REPRODUCED: $*" >&2; finish 1; }
pass_ok() { echo "DEMAND_OK: $*" >&2; finish 0; }

record_pid() {
  _p=$1
  [ -n "$_p" ] || return 0
  case " $PIDS " in
    *" $_p "*) return 0 ;;
  esac
  PIDS="$PIDS $_p"
}

narrow_ps() {
  ps -o pid,ppid,etime,stat,comm -p "$1" 2>/dev/null || true
}

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

load1() {
  /usr/sbin/sysctl -n vm.loadavg 2>/dev/null | awk '{gsub(/[{}]/,""); print $1}' || true
}

say() { printf '%s\n' "$*"; }

finish() {
  _rc=$1
  NOW=$(date +%s)
  ELAP=$((NOW - STARTED))
  say "RESULT rc=$_rc elapsed=${ELAP}s load_after=$(load1) bin=$TA md5=$BIN_MD5"
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
        *ta-d126-stall-*) rm -rf "$RUN" ;;
      esac
    fi
  fi
  exit "$_rc"
}

trap 'unjudgeable interrupted' INT TERM

LOAD_BEFORE=$(load1)
say "LOAD before=$LOAD_BEFORE"

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
[ -x "$NODE_BIN" ] || unjudgeable "node missing: $NODE_BIN"
TMUX_BIN="$REAL_TMUX"

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$BINDIR" || unjudgeable "mkdir failed"
TA="$BINDIR/team-agent"
cp "$SRC_BIN" "$TA" || unjudgeable "cp gauge failed"
chmod +x "$TA"
BIN_MD5=$(md5 -q "$TA" 2>/dev/null || md5sum "$TA" | awk '{print $1}')
BIN_MTIME=$(stat -f '%Sm' "$TA" 2>/dev/null || stat -c '%y' "$TA" 2>/dev/null || echo unknown)
say "GAUGE path=$TA md5=$BIN_MD5 mtime=$BIN_MTIME version=$("$TA" --version 2>/dev/null || true)"

cp "$NODE/drop-enter.js" "$RUN/drop-enter.js" || unjudgeable "copy drop-enter.js failed"
SIDE="$RUN/tui.side"
ENTERLOG="$RUN/tui.enter"
: >"$ENTERLOG"

cat > "$TEAMDIR/TEAM.md" <<EOF
---
name: $TEAM_ID
objective: stall-resend-noncopy probe
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
  COORD_PID=$(tr -d ' \n' < "$WS/.team/runtime/coordinator.pid")
  record_pid "$COORD_PID"
fi

SOCK=$(python3 - "$WS/.team/runtime/state.json" <<'PY'
import json, sys, os
d = json.load(open(sys.argv[1]))
print(os.path.basename(d.get("tmux_socket") or ""))
PY
)
[ -n "$SOCK" ] || unjudgeable "empty tmux_socket"
protected_name "$SOCK" && unjudgeable "workspace socket is protected: $SOCK"
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
say "W1=$W1"

"$TMUX_BIN" -L "$SOCK" resize-pane -t "$W1" -x 120 -y 24 >/dev/null 2>&1 || true
# dropFirst=5：只丢 C-m。157ae109 本装置实测 enter_n=5（主循环+捕获重试路径），
# 修前无第 6 颗；修后 Unverified 补一颗才写 side。
"$TMUX_BIN" -L "$SOCK" respawn-pane -k -t "$W1" -- "$NODE_BIN" "$RUN/drop-enter.js" "$SIDE" "$ENTERLOG" 0 5
sleep 1
WPID=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1" '#{pane_pid}' 2>/dev/null || true)
record_pid "$WPID"

t=0
while [ "$t" -lt 15 ]; do
  cap=$("$TMUX_BIN" -L "$SOCK" capture-pane -p -t "$W1" 2>/dev/null || true)
  case "$cap" in
    *tui-ready*) break ;;
  esac
  sleep 1
  t=$((t + 1))
done
case "$cap" in
  *tui-ready*) say "drop-enter ready" ;;
  *) unjudgeable "drop-enter never printed tui-ready" ;;
esac

MARK="STALLD126-$$"
# 长行逼 tmux 80 列把 token 折开，命中 should_resubmit 假阴。
PAYLOAD="STALLRESEND ${MARK} $(printf 'x%.0s' $(seq 1 70))"
EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
UNVER_B=0
[ -n "$EVENTS" ] && [ -f "$EVENTS" ] && UNVER_B=$(grep -c -F '"event": "send.unverified"' "$EVENTS" 2>/dev/null || true)

"$TA" send "$AGENT_ID" "$PAYLOAD" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send.json" 2>"$RUN/send.err" || unjudgeable "send failed"
say "SEND $(cat "$RUN/send.json" 2>/dev/null | tr '\n' ' ')"

w=0
SIDE_N=0
while [ "$w" -lt 25 ]; do
  NOW=$(date +%s)
  [ $((NOW - STARTED)) -ge "$BUDGET_SEC" ] && unjudgeable "budget"
  if [ -f "$SIDE" ]; then
    SIDE_N=$(awk 'END {print NR+0}' "$SIDE")
  fi
  ENTER_N=0
  [ -f "$ENTERLOG" ] && ENTER_N=$(awk 'END {print NR+0}' "$ENTERLOG")
  say "t=${w}s side_lines=$SIDE_N enter_n=$ENTER_N"
  [ "$SIDE_N" -gt 0 ] && break
  sleep 1
  w=$((w + 1))
done

UNVER_A=0
[ -n "$EVENTS" ] && [ -f "$EVENTS" ] && UNVER_A=$(grep -c -F '"event": "send.unverified"' "$EVENTS" 2>/dev/null || true)
DELTA_U=$((UNVER_A - UNVER_B))
say "delta_unverified=$DELTA_U side_lines=$SIDE_N enter_n=$ENTER_N"

if [ "$SIDE_N" -gt 0 ]; then
  pass_ok "drop-enter consumed after framework resend side_lines=$SIDE_N enter_n=$ENTER_N delta_unverified=$DELTA_U"
fi
if [ "$DELTA_U" -gt 0 ] && [ "$SIDE_N" -eq 0 ]; then
  fail_repro "Unverified and no submit in ${w}s (zero composer resend) enter_n=$ENTER_N"
fi
unjudgeable "did not reach Unverified and did not submit; side=$SIDE_N delta_u=$DELTA_U enter_n=$ENTER_N"
