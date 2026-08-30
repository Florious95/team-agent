#!/usr/bin/env bash
# purpose: Q3 复现装置——#1 还活着时同目录起 launcher#2,无法并入同一 tmux 平面
# contract: exit 0=绿=两个 leader pane 都活且同一 socket(list-panes 数量>=2 + socket 名一致 + 两 launcher pid 都活);
#           exit 1=红=#2 被拒/报错/另起 socket/挤掉 #1;
#           exit 2=不可判。自清理本装置 socket/目录。
# boundary: 只在 mktemp 隔离目录; 禁裸 tmux; 禁碰保护 socket; ps 窄字段;
#           只杀本装置记录的精确 PID。不依赖 provider 长跑。
#
# 预算: 首启 FIRST_WAIT(默认 90s) + 二次 SECOND_WAIT(默认 40s) + 清理。
# 高载超时进 2,不把超时折成红。
#
# Q3 ≠ Q1: 必须在送出 #2 argv 的那一刻证明 #1 launcher pid 仍在 ps。
# #1 已死再起是 Q1/residue,本装置记 2,不记红。

set -u

DEFAULT_GAUGE="/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent"
DEFAULT_MD5="43d8ed96f697b2bac0bfcc5e70f71faf"
GAUGE="${REPRO_GAUGE:-$DEFAULT_GAUGE}"
EXPECT_MD5="${REPRO_GAUGE_MD5:-}"
if [[ -z "${REPRO_GAUGE:-}" ]]; then
  EXPECT_MD5="$DEFAULT_MD5"
fi
FIRST_WAIT="${REPRO_FIRST_WAIT:-90}"
SECOND_WAIT="${REPRO_SECOND_WAIT:-40}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
export PATH="/Users/alauda/ta-tmux-shim/bin:/opt/homebrew/bin:/Users/alauda/.local/bin:/usr/bin:/bin:${PATH:-}"
unset TMUX TMUX_PANE

TMUX_BIN="$(command -v tmux || true)"
tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

CTRL=""
WS=""
L1_PID=""
L1_ATTACH=""
L2_PID=""
L2_ATTACH=""
WS_SOCK=""
WS_SOCK2=""
PS_SNAP=""
SOCK_PRE=""
CLEANED=0

say() { printf '%s\n' "$*"; }
protected() { [[ "$PROTECTED" == *" $1 "* ]]; }
die2() { say "UNJUDGEABLE: $*"; exit 2; }

rm_stale_sock() {
  local name="$1"
  [[ -n "$name" ]] || return 0
  protected "$name" && return 0
  "$TMUX_BIN" -L "$name" kill-server >/dev/null 2>&1 || true
  if "$TMUX_BIN" -L "$name" list-sessions >/dev/null 2>&1; then
    return 0
  fi
  rm -f "/private/tmp/tmux-501/$name" "/tmp/tmux-501/$name" 2>/dev/null || true
}

kill_recorded() {
  local pid="$1"
  [[ -n "$pid" ]] || return 0
  if ps -p "$pid" -o pid= >/dev/null 2>&1; then
    kill -TERM "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  [[ "$CLEANED" -eq 1 ]] && return
  CLEANED=1
  for pid in "${L2_PID:-}" "${L2_ATTACH:-}" "${L1_PID:-}" "${L1_ATTACH:-}"; do
    kill_recorded "$pid"
  done
  sleep 1
  for pid in "${L2_PID:-}" "${L2_ATTACH:-}" "${L1_PID:-}" "${L1_ATTACH:-}"; do
    [[ -n "$pid" ]] || continue
    if ps -p "$pid" -o pid= >/dev/null 2>&1; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
  rm_stale_sock "${WS_SOCK2:-}"
  rm_stale_sock "${WS_SOCK:-}"
  rm_stale_sock "${CTRL:-}"
  if [[ -n "${WS:-}" ]]; then
    case "$WS" in
      /tmp/ta-t114-*|/private/tmp/ta-t114-*) rm -rf "$WS" ;;
    esac
  fi
  if [[ -n "${PS_SNAP:-}" ]]; then
    case "$PS_SNAP" in
      /tmp/ta-t114-ps-*|/private/tmp/ta-t114-ps-*) rm -f "$PS_SNAP" ;;
    esac
  fi
  if [[ -n "${SOCK_PRE:-}" ]]; then
    case "$SOCK_PRE" in
      /tmp/ta-t114-pre-*|/private/tmp/ta-t114-pre-*) rm -f "$SOCK_PRE" ;;
    esac
  fi
}
trap 'cleanup' EXIT INT TERM

alive_pid() {
  local pid="$1"
  [[ -n "$pid" ]] || return 1
  ps -p "$pid" -o pid= >/dev/null 2>&1
}

comm_of() {
  ps -p "$1" -o comm= 2>/dev/null || true
}

find_ta_under() {
  local root="$1"
  local comm pid ppid _etime _stat commrest cur
  comm="$(comm_of "$root")"
  case "$comm" in
    *team-agent*) printf '%s\n' "$root"; return 0 ;;
  esac
  ps -ax -o pid,ppid,etime,stat,comm > "$PS_SNAP" 2>/dev/null || true
  while read -r pid ppid _etime _stat commrest; do
    [[ "$ppid" == "$root" ]] || continue
    case "$commrest" in
      *team-agent*) printf '%s\n' "$pid"; return 0 ;;
    esac
  done < "$PS_SNAP"
  # Success path: pane_pid becomes the attach client; launcher is its parent.
  cur="$root"
  local i
  for i in 1 2 3 4 5 6; do
    ppid="$(ps -p "$cur" -o ppid= 2>/dev/null | tr -d ' ')"
    [[ -n "$ppid" && "$ppid" != "0" && "$ppid" != "1" ]] || break
    comm="$(comm_of "$ppid")"
    case "$comm" in
      *team-agent*) printf '%s\n' "$ppid"; return 0 ;;
    esac
    cur="$ppid"
  done
  return 1
}

find_tmux_child() {
  local parent="$1"
  local pid ppid _etime _stat commrest
  ps -ax -o pid,ppid,etime,stat,comm > "$PS_SNAP" 2>/dev/null || true
  while read -r pid ppid _etime _stat commrest; do
    [[ "$ppid" == "$parent" ]] || continue
    case "$commrest" in
      *tmux*) printf '%s\n' "$pid"; return 0 ;;
    esac
  done < "$PS_SNAP"
  return 1
}

read_sock_from_state() {
  python3 - "$1" <<'PY'
import json, sys
from os.path import basename
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
if "tmux_socket" not in d:
    raise SystemExit(3)
print(basename(d["tmux_socket"]))
PY
}

read_receiver() {
  python3 - "$1" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
if "teams" not in d:
    raise SystemExit(3)
teams = d["teams"]
if "current" not in teams:
    raise SystemExit(3)
cur = teams["current"]
if "leader_receiver" not in cur:
    raise SystemExit(3)
lr = cur["leader_receiver"]
for k in ("session_name", "window_name", "pane_id"):
    if k not in lr:
        raise SystemExit(3)
print(lr["session_name"])
print(lr["window_name"])
print(lr["pane_id"])
PY
}

if [[ -z "$TMUX_BIN" || ! -x "$TMUX_BIN" ]]; then
  die2 "tmux not on PATH"
fi
if [[ ! -x "$GAUGE" ]]; then
  die2 "gauge missing: $GAUGE"
fi
GOT_MD5="$(md5 -q "$GAUGE" 2>/dev/null || true)"
if [[ -n "$EXPECT_MD5" && "$GOT_MD5" != "$EXPECT_MD5" ]]; then
  die2 "gauge md5 mismatch got=${GOT_MD5:-empty} expect=$EXPECT_MD5 path=$GAUGE"
fi
MTIME="$(stat -f '%Sm' "$GAUGE")"
SIZE="$(stat -f '%z' "$GAUGE")"
VER="$("$GAUGE" --version 2>/dev/null || true)"
say "GAUGE path=$GAUGE md5=$GOT_MD5 mtime=$MTIME size=$SIZE version=$VER"
say "FIRST_WAIT=$FIRST_WAIT SECOND_WAIT=$SECOND_WAIT"
say "UPTIME=$(uptime)"

WS="$(mktemp -d /tmp/ta-t114-XXXXXX)"
git -C "$WS" init -q
say "WS=$WS"
PS_SNAP="$(mktemp /tmp/ta-t114-ps-XXXXXX)"
SOCK_PRE="$(mktemp /tmp/ta-t114-pre-XXXXXX)"
ls /private/tmp/tmux-501 2>/dev/null | sort > "$SOCK_PRE" || true

CTRL="t114j$$"
if protected "$CTRL"; then
  die2 "ctrl socket name collided with protected: $CTRL"
fi

"$TMUX_BIN" -L "$CTRL" new-session -d -s ctrl -n l1 -x 140 -y 40 \
  "unset TMUX TMUX_PANE; cd '$WS' || exit 91; exec '$GAUGE' claude"
"$TMUX_BIN" -L "$CTRL" new-window -d -n l2
sleep 0.4
L1_PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:l1 '#{pane_pid}' 2>/dev/null || true)"
[[ -n "$L1_PANE_PID" ]] || die2 "control l1 pane pid empty"
say "CTRL=$CTRL L1_PANE_PID=$L1_PANE_PID"

t=0
L1_PID=""
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  L1_PID="$(find_ta_under "$L1_PANE_PID" || true)"
  [[ -n "$L1_PID" ]] && break
  sleep 1
  t=$((t + 1))
done
[[ -n "$L1_PID" ]] || die2 "launcher#1 pid not found within ${FIRST_WAIT}s"

t=0
L1_ATTACH=""
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  if [[ -z "$L1_ATTACH" ]]; then
    L1_ATTACH="$(find_tmux_child "$L1_PID" || true)"
  fi
  if [[ -f "$WS/.team/runtime/state.json" && -z "$WS_SOCK" ]]; then
    WS_SOCK="$(read_sock_from_state "$WS/.team/runtime/state.json" 2>/dev/null || true)"
  fi
  if [[ -n "$WS_SOCK" ]] && protected "$WS_SOCK"; then
    die2 "workspace socket is protected: $WS_SOCK — refusing to touch"
  fi
  if [[ -n "$L1_ATTACH" && -n "$WS_SOCK" ]]; then
    wins="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -F '#{window_name}' 2>/dev/null || true)"
    if printf '%s\n' "$wins" | grep -qx 'claude_code'; then
      break
    fi
  fi
  sleep 1
  t=$((t + 1))
done
say "L1_PID=$L1_PID L1_ATTACH=${L1_ATTACH:-empty} WS_SOCK=${WS_SOCK:-empty} waited=${t}s"
[[ -n "$L1_ATTACH" ]] || die2 "attach child of #1 not observed; first launch not on managed-attach path"
[[ -n "$WS_SOCK" ]] || die2 "workspace tmux_socket never appeared"

RECV="$(read_receiver "$WS/.team/runtime/state.json" 2>/dev/null || true)"
[[ -n "$RECV" ]] || die2 "state.json missing leader_receiver keys after #1"
SESS_NAME="$(printf '%s\n' "$RECV" | sed -n '1p')"
WIN_NAME="$(printf '%s\n' "$RECV" | sed -n '2p')"
PANE_ID="$(printf '%s\n' "$RECV" | sed -n '3p')"
say "L1_SESS=$SESS_NAME L1_WIN=$WIN_NAME L1_PANE=$PANE_ID"

cp "$WS/.team/runtime/state.json" "$WS/.t114-state-after-l1.json"

say "=== BEFORE_L2 list-sessions ==="
"$TMUX_BIN" -L "$WS_SOCK" list-sessions 2>&1 || true
say "=== BEFORE_L2 list-windows ==="
"$TMUX_BIN" -L "$WS_SOCK" list-windows 2>&1 || true
say "=== BEFORE_L2 list-panes -a ==="
"$TMUX_BIN" -L "$WS_SOCK" list-panes -a -F 'pane=#{pane_id} win=#{window_name} sess=#{session_name} pid=#{pane_pid}' 2>&1 || true
say "=== BEFORE_L2 list-windows -t ${SESS_NAME}:${WIN_NAME} (positive control, one window must parse) ==="
"$TMUX_BIN" -L "$WS_SOCK" list-windows -t "${SESS_NAME}:${WIN_NAME}" 2>&1 || true
PANE_COUNT_BEFORE="$("$TMUX_BIN" -L "$WS_SOCK" list-panes -a -F '#{pane_id}' 2>/dev/null | sed '/^$/d' | wc -l | tr -d ' ')"
say "PANE_COUNT_BEFORE=$PANE_COUNT_BEFORE"

if ! alive_pid "$L1_PID"; then
  die2 "launcher#1 pid $L1_PID died before #2 start; cannot claim Q3 (#1 still alive)"
fi
L1_STAT_AT_L2="$(ps -p "$L1_PID" -o pid,ppid,etime,stat,comm 2>/dev/null || true)"
say "L1_ALIVE_AT_L2_START=yes $L1_STAT_AT_L2"

L2_RC_FILE="$WS/.t114-l2.rc"
# Respawn (not send-keys): pane_pid stays the wrapper shell, child is the
# launcher. exec would drop stderr on the old red path (pane dies empty).
say "L2_START=$(date '+%H:%M:%S')"
"$TMUX_BIN" -L "$CTRL" respawn-pane -k -t ctrl:l2 \
  "unset TMUX TMUX_PANE; cd '$WS' || exit 91; echo T114_L2_START=\$(date '+%H:%M:%S'); '$GAUGE' claude; echo \$? > '$L2_RC_FILE'; echo T114_L2_DONE=\$(date '+%H:%M:%S')"

t=0
L2_PANE=""
while [[ "$t" -lt "$SECOND_WAIT" ]]; do
  L2_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:l2 -S -80 2>/dev/null || true)"
  L2_PANE_PID_NOW="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:l2 '#{pane_pid}' 2>/dev/null || true)"
  if [[ -n "$L2_PANE_PID_NOW" && -z "$L2_PID" ]]; then
    L2_PID="$(find_ta_under "$L2_PANE_PID_NOW" || true)"
  fi
  if [[ -n "$L2_PID" && -z "$L2_ATTACH" ]]; then
    L2_ATTACH="$(find_tmux_child "$L2_PID" || true)"
  fi
  if [[ -f "$WS/.team/runtime/state.json" ]]; then
    WS_SOCK2="$(read_sock_from_state "$WS/.team/runtime/state.json" 2>/dev/null || true)"
  fi
  if [[ -f "$L2_RC_FILE" ]]; then
    break
  fi
  if [[ -n "$L2_PID" ]] && alive_pid "$L2_PID" && alive_pid "$L1_PID"; then
    wins_now="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -F '#{window_name}' 2>/dev/null || true)"
    n_win="$(printf '%s\n' "$wins_now" | sed '/^$/d' | wc -l | tr -d ' ')"
    n_uniq="$(printf '%s\n' "$wins_now" | sed '/^$/d' | sort -u | wc -l | tr -d ' ')"
    # Duplicate claude_code windows are the red collision, not a join.
    if [[ "${n_win:-0}" -ge 2 && "$n_win" == "$n_uniq" ]]; then
      break
    fi
  fi
  if printf '%s\n' "$L2_PANE" | grep -F -q "can't find window: claude_code"; then
    sleep 1
    L2_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:l2 -S -80 2>/dev/null || true)"
    break
  fi
  if printf '%s\n' "$L2_PANE" | grep -F -q "tmux is not installed"; then
    die2 "probe PATH missing tmux (not product red): $L2_PANE"
  fi
  sleep 1
  t=$((t + 1))
done
L2_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:l2 -S -80 2>/dev/null || true)"
say "L2_WAITED=${t}s L2_PID=${L2_PID:-empty} L2_ATTACH=${L2_ATTACH:-empty} WS_SOCK2=${WS_SOCK2:-empty}"
say "=== L2_PANE ==="
say "$L2_PANE"
if [[ -f "$L2_RC_FILE" ]]; then
  say "T114_L2_RC_FILE=$(cat "$L2_RC_FILE")"
fi

say "=== AFTER_L2 list-sessions ==="
"$TMUX_BIN" -L "$WS_SOCK" list-sessions 2>&1 || true
say "=== AFTER_L2 list-windows ==="
"$TMUX_BIN" -L "$WS_SOCK" list-windows 2>&1 || true
say "=== AFTER_L2 list-panes -a ==="
"$TMUX_BIN" -L "$WS_SOCK" list-panes -a -F 'pane=#{pane_id} win=#{window_name} sess=#{session_name} pid=#{pane_pid}' 2>&1 || true
say "=== AFTER_L2 list-windows -t ${SESS_NAME}:${WIN_NAME} ==="
"$TMUX_BIN" -L "$WS_SOCK" list-windows -t "${SESS_NAME}:${WIN_NAME}" 2>&1 || true
PANE_COUNT_AFTER="$("$TMUX_BIN" -L "$WS_SOCK" list-panes -a -F '#{pane_id}' 2>/dev/null | sed '/^$/d' | wc -l | tr -d ' ')"
say "PANE_COUNT_AFTER=$PANE_COUNT_AFTER"

L1_ALIVE_AFTER=no
if alive_pid "$L1_PID"; then
  L1_ALIVE_AFTER=yes
fi
say "L1_ALIVE_AFTER=$L1_ALIVE_AFTER"
ps -p "$L1_PID" -o pid,ppid,etime,stat,comm 2>/dev/null || true

L2_ALIVE=no
if [[ -n "${L2_PID:-}" ]] && alive_pid "$L2_PID"; then
  L2_ALIVE=yes
  ps -p "$L2_PID" -o pid,ppid,etime,stat,comm 2>/dev/null || true
fi
say "L2_ALIVE=$L2_ALIVE"

if [[ -f "$WS/.team/logs/events.jsonl" ]]; then
  say "=== events.jsonl ==="
  cat "$WS/.team/logs/events.jsonl"
fi
if [[ -d "$WS/.team/logs" ]]; then
  say "=== logs names ==="
  ls -1 "$WS/.team/logs" || true
fi

# extra sockets created during this run (not pre-existing, not protected, not CTRL)
SOCK_POST="$(mktemp /tmp/ta-t114-post-XXXXXX)"
ls /private/tmp/tmux-501 2>/dev/null | sort > "$SOCK_POST" || true
say "=== NEW_SOCKETS vs pre ==="
comm -13 "$SOCK_PRE" "$SOCK_POST" || true
rm -f "$SOCK_POST"

FILE_RC=""
if [[ -f "$L2_RC_FILE" ]]; then
  FILE_RC="$(cat "$L2_RC_FILE")"
  FILE_RC="${FILE_RC//$'\n'/}"
fi

# --- 裁定 ---
# 绿: 两 launcher 活 + 同一 socket + pane 数>=2
SAME_SOCK=yes
if [[ -n "${WS_SOCK2:-}" && "$WS_SOCK2" != "$WS_SOCK" ]]; then
  SAME_SOCK=no
fi
say "SAME_SOCK=$SAME_SOCK WS_SOCK=$WS_SOCK WS_SOCK2=${WS_SOCK2:-empty}"

if [[ "$L1_ALIVE_AFTER" == "yes" && "$L2_ALIVE" == "yes" && "$SAME_SOCK" == "yes" && "$PANE_COUNT_AFTER" -ge 2 ]]; then
  say "GREEN: two live launchers + same socket + pane_count=$PANE_COUNT_AFTER"
  exit 0
fi

# 挤掉 #1: 送 #2 时还活,之后死,且本装置没先杀它
if [[ "$L1_ALIVE_AFTER" != "yes" ]]; then
  say "RED: launcher#1 pid $L1_PID was alive at #2 start, dead after #2 (kicked)"
  exit 1
fi

# 另起 socket
if [[ "$SAME_SOCK" != "yes" ]]; then
  say "RED: #2 used different socket WS_SOCK=$WS_SOCK WS_SOCK2=$WS_SOCK2"
  exit 1
fi

# 被拒原文
if printf '%s\n' "$L2_PANE" | grep -F -q "can't find window: claude_code"; then
  say "RED: #2 refused with can't find window: claude_code; pane_count=$PANE_COUNT_AFTER (want >=2)"
  exit 1
fi

# 其它 leader start 报错(非环境)
if printf '%s\n' "$L2_PANE" | grep -F -q "leader start error"; then
  say "RED: #2 leader start error (not join); pane_count=$PANE_COUNT_AFTER"
  exit 1
fi
if [[ -n "$FILE_RC" && "$FILE_RC" != "0" ]]; then
  say "RED: #2 exited rc=$FILE_RC without joining; pane_count=$PANE_COUNT_AFTER"
  exit 1
fi

# #2 仍活但没并入第二 pane: 未完成,不可判(可能卡住)
if [[ "$L2_ALIVE" == "yes" && "$PANE_COUNT_AFTER" -lt 2 ]]; then
  die2 "#2 launcher still alive but pane_count=$PANE_COUNT_AFTER < 2 within ${SECOND_WAIT}s"
fi

die2 "#2 produced neither join nor refusal/error/rc (L2_ALIVE=$L2_ALIVE FILE_RC=${FILE_RC:-empty} pane_count=$PANE_COUNT_AFTER)"
