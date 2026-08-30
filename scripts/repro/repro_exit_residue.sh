#!/usr/bin/env bash
# purpose: Q1 复现装置——退出后同目录再起撞「can't find window: claude_code」
# contract: exit 1=红=复现成功(二次启动原文含 can't find window: claude_code);
#           exit 0=绿=二次启动未撞该拒绝; exit 2=不可判。自清理本装置 socket/目录。
# boundary: 只在 mktemp 隔离目录; 禁裸 tmux; 禁碰保护 socket; ps 窄字段;
#           只杀本装置记录的精确 PID。
#
# 「退出」: 任务书要求 send-keys C-c×2 后同目录再起。调查报告已证 C-c×2
# 并不让 launcher 落回 shell。装置仍发送 C-c×2,然后对记录的 launcher PID
# 发 SIGTERM 完成「退出后」(与调查 Q1 路径相同)。这不是把 Q1 改成 Q3。
#
# 红只认拒绝原文,其它失败进 2。预算: 首启 75s + 重试 25s。

set -u

DEFAULT_GAUGE="/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent"
DEFAULT_MD5="2b7cf51937ea2d50897eeba75fae3b6b"
GAUGE="${REPRO_GAUGE:-$DEFAULT_GAUGE}"
EXPECT_MD5="${REPRO_GAUGE_MD5:-}"
if [[ -z "${REPRO_GAUGE:-}" ]]; then
  EXPECT_MD5="$DEFAULT_MD5"
fi
FIRST_WAIT="${REPRO_FIRST_WAIT:-75}"
RETRY_WAIT="${REPRO_RETRY_WAIT:-25}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
export PATH="/Users/alauda/ta-tmux-shim/bin:/opt/homebrew/bin:/Users/alauda/.local/bin:/usr/bin:/bin:${PATH:-}"
unset TMUX TMUX_PANE

TMUX_BIN="$(command -v tmux || true)"
tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

CTRL=""
WS=""
LAUNCHER_PID=""
ATTACH_PID=""
RETRY_PID=""
WS_SOCK=""
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

cleanup() {
  [[ "$CLEANED" -eq 1 ]] && return
  CLEANED=1
  for pid in "${RETRY_PID:-}" "${LAUNCHER_PID:-}" "${ATTACH_PID:-}"; do
    [[ -n "$pid" ]] || continue
    if ps -p "$pid" -o pid= >/dev/null 2>&1; then
      kill -TERM "$pid" 2>/dev/null || true
    fi
  done
  sleep 1
  for pid in "${RETRY_PID:-}" "${LAUNCHER_PID:-}" "${ATTACH_PID:-}"; do
    [[ -n "$pid" ]] || continue
    if ps -p "$pid" -o pid= >/dev/null 2>&1; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
  rm_stale_sock "${WS_SOCK:-}"
  rm_stale_sock "${CTRL:-}"
  if [[ -n "${WS:-}" ]]; then
    case "$WS" in
      /tmp/ta-t103-*|/private/tmp/ta-t103-*) rm -rf "$WS" ;;
    esac
  fi
}
trap 'cleanup' EXIT INT TERM

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
say "GAUGE path=$GAUGE md5=$GOT_MD5 mtime=$(stat -f '%Sm' "$GAUGE") size=$(stat -f '%z' "$GAUGE") version=$("$GAUGE" --version 2>/dev/null || true)"

WS="$(mktemp -d /tmp/ta-t103-XXXXXX)"
git -C "$WS" init -q
say "WS=$WS"

CTRL="t103q$$"
if protected "$CTRL"; then
  die2 "ctrl socket name collided with protected: $CTRL"
fi

"$TMUX_BIN" -L "$CTRL" new-session -d -s ctrl -n launch -x 140 -y 40 \
  "unset TMUX TMUX_PANE; cd '$WS' || exit 91; exec '$GAUGE' claude"
"$TMUX_BIN" -L "$CTRL" new-window -d -n retry
sleep 0.4
PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:launch '#{pane_pid}' 2>/dev/null || true)"
[[ -n "$PANE_PID" ]] || die2 "control pane pid empty"
say "CTRL=$CTRL PANE_PID=$PANE_PID"

t=0
LAUNCHER_PID=""
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  comm="$(ps -p "$PANE_PID" -o comm= 2>/dev/null || true)"
  case "$comm" in
    *team-agent*) LAUNCHER_PID="$PANE_PID"; break ;;
  esac
  while read -r pid ppid _etime _stat commrest; do
    [[ "$ppid" == "$PANE_PID" ]] || continue
    case "$commrest" in
      *team-agent*) LAUNCHER_PID="$pid"; break ;;
    esac
  done < <(ps -ax -o pid,ppid,etime,stat,comm)
  [[ -n "$LAUNCHER_PID" ]] && break
  sleep 1
  t=$((t + 1))
done
[[ -n "$LAUNCHER_PID" ]] || die2 "launcher pid not found within ${FIRST_WAIT}s"

t=0
ATTACH_PID=""
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  if [[ -z "$ATTACH_PID" ]]; then
    while read -r pid ppid _etime _stat commrest; do
      [[ "$ppid" == "$LAUNCHER_PID" ]] || continue
      case "$commrest" in
        *tmux*) ATTACH_PID="$pid"; break ;;
      esac
    done < <(ps -ax -o pid,ppid,etime,stat,comm)
  fi
  if [[ -f "$WS/.team/runtime/state.json" && -z "$WS_SOCK" ]]; then
    WS_SOCK="$(python3 - "$WS/.team/runtime/state.json" <<'PY'
import json, sys
from os.path import basename
with open(sys.argv[1]) as f:
    d = json.load(f)
if "tmux_socket" not in d:
    raise SystemExit(3)
print(basename(d["tmux_socket"]))
PY
)" || true
  fi
  if [[ -n "$WS_SOCK" ]] && protected "$WS_SOCK"; then
    die2 "workspace socket is protected: $WS_SOCK — refusing to touch"
  fi
  if [[ -n "$ATTACH_PID" && -n "$WS_SOCK" ]]; then
    wins="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -F '#{window_name}' 2>/dev/null || true)"
    if printf '%s\n' "$wins" | grep -qx 'claude_code'; then
      break
    fi
  fi
  sleep 1
  t=$((t + 1))
done
say "LAUNCHER_PID=$LAUNCHER_PID ATTACH_PID=${ATTACH_PID:-empty} WS_SOCK=${WS_SOCK:-empty}"
[[ -n "$ATTACH_PID" ]] || die2 "attach child not observed; first launch not on managed-attach path"
[[ -n "$WS_SOCK" ]] || die2 "workspace tmux_socket never appeared"

SESS="$(python3 - "$WS/.team/runtime/state.json" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
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
)" || die2 "state.json missing leader_receiver keys"
SESS_NAME="$(printf '%s\n' "$SESS" | sed -n '1p')"
WIN_NAME="$(printf '%s\n' "$SESS" | sed -n '2p')"
PANE_ID="$(printf '%s\n' "$SESS" | sed -n '3p')"
say "SESS=$SESS_NAME WIN=$WIN_NAME PANE=$PANE_ID"

say "SEND_CC=$(date '+%H:%M:%S')"
"$TMUX_BIN" -L "$CTRL" send-keys -t ctrl:launch C-c
sleep 0.4
"$TMUX_BIN" -L "$CTRL" send-keys -t ctrl:launch C-c
sleep 1
say "TERM_LAUNCHER pid=$LAUNCHER_PID at $(date '+%H:%M:%S')"
kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
t=0
while [[ "$t" -lt 15 ]]; do
  ps -p "$LAUNCHER_PID" -o pid= >/dev/null 2>&1 || break
  sleep 1
  t=$((t + 1))
done
if ps -p "$LAUNCHER_PID" -o pid= >/dev/null 2>&1; then
  die2 "launcher pid $LAUNCHER_PID did not die after SIGTERM (cannot reach 退出后)"
fi
say "LAUNCHER_DEAD after ${t}s"

say "=== LEFTOVER AFTER EXIT ==="
say "socket_path=/private/tmp/tmux-501/$WS_SOCK"
if [[ -S "/private/tmp/tmux-501/$WS_SOCK" ]]; then
  say "socket_exists=yes"
else
  say "socket_exists=no"
fi
say "list-sessions:"
"$TMUX_BIN" -L "$WS_SOCK" list-sessions 2>&1 || true
say "list-windows:"
"$TMUX_BIN" -L "$WS_SOCK" list-windows 2>&1 || true
say "list-windows -t ${SESS_NAME}:${WIN_NAME}:"
"$TMUX_BIN" -L "$WS_SOCK" list-windows -t "${SESS_NAME}:${WIN_NAME}" 2>&1 || true
say "runtime tree:"
find "$WS/.team" -maxdepth 3 \( -type f -o -type s \) 2>/dev/null | sort || true
if [[ -f "$WS/.team/runtime/state.json" ]]; then
  say "state.json:"
  cat "$WS/.team/runtime/state.json"
fi

# 短 send-keys,RC 写文件,避免长命令折行,也避免 capture 命中「echo RC=」那一行。
RETRY_SH="$WS/.t103-retry.sh"
RETRY_RC_FILE="$WS/.t103-retry.rc"
cat > "$RETRY_SH" <<EOF
#!/usr/bin/env bash
unset TMUX TMUX_PANE
cd '$WS' || exit 91
echo T103_RETRY_START=\$(date '+%H:%M:%S')
'$GAUGE' claude
echo \$? > '$RETRY_RC_FILE'
echo T103_RETRY_DONE=\$(date '+%H:%M:%S')
EOF
chmod +x "$RETRY_SH"
say "Q1_RETRY_START=$(date '+%H:%M:%S')"
"$TMUX_BIN" -L "$CTRL" send-keys -t ctrl:retry "bash '$RETRY_SH'" C-m

t=0
RETRY_PANE=""
while [[ "$t" -lt "$RETRY_WAIT" ]]; do
  RETRY_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:retry -S -50 2>/dev/null || true)"
  if [[ -f "$RETRY_RC_FILE" ]]; then
    break
  fi
  if printf '%s\n' "$RETRY_PANE" | grep -F -q "can't find window: claude_code"; then
    sleep 1
    RETRY_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:retry -S -50 2>/dev/null || true)"
    break
  fi
  sleep 1
  t=$((t + 1))
done
RETRY_PANE="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:retry -S -50 2>/dev/null || true)"
RETRY_TEXT="$RETRY_PANE"
say "=== Q1_RETRY_PANE ==="
say "$RETRY_PANE"
say "Q1_RETRY_WAITED=${t}s"
if [[ -f "$RETRY_RC_FILE" ]]; then
  say "T103_RETRY_RC_FILE=$(cat "$RETRY_RC_FILE")"
fi

if [[ -f "$WS/.team/logs/events.jsonl" ]]; then
  say "=== events.jsonl ==="
  cat "$WS/.team/logs/events.jsonl"
fi

COMBINED="${RETRY_TEXT}
${RETRY_PANE}"
if printf '%s\n' "$COMBINED" | grep -F -q "can't find window: claude_code"; then
  say "RED: second start refused with can't find window: claude_code"
  exit 1
fi

FILE_RC=""
if [[ -f "$RETRY_RC_FILE" ]]; then
  FILE_RC="$(cat "$RETRY_RC_FILE")"
  FILE_RC="${FILE_RC//$'\n'/}"
fi
if [[ "$FILE_RC" == "0" ]]; then
  say "GREEN: second start RC=0"
  exit 0
fi
if [[ -n "$FILE_RC" ]]; then
  die2 "second start failed but not the Q1 refusal (T103_RETRY_RC=$FILE_RC)"
fi

# 没返回也没拒绝原文: 可能卡在 attach(修好后的形态之一) → 绿
RETRY_PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:retry '#{pane_pid}' 2>/dev/null || true)"
alive_ta=""
is_ta() {
  local comm
  comm="$(ps -p "$1" -o comm= 2>/dev/null || true)"
  case "$comm" in
    *team-agent*) return 0 ;;
  esac
  return 1
}
if [[ -n "$RETRY_PANE_PID" ]]; then
  if is_ta "$RETRY_PANE_PID"; then
    alive_ta="$RETRY_PANE_PID"
    RETRY_PID="$RETRY_PANE_PID"
  else
    RETRY_PPID="$(ps -p "$RETRY_PANE_PID" -o ppid= 2>/dev/null | tr -d ' ')"
    if [[ -n "$RETRY_PPID" ]] && is_ta "$RETRY_PPID"; then
      alive_ta="$RETRY_PPID"
      RETRY_PID="$RETRY_PPID"
    fi
  fi
  if [[ -z "$alive_ta" ]]; then
    while read -r pid ppid _etime _stat commrest; do
      [[ "$ppid" == "$RETRY_PANE_PID" || "$ppid" == "${RETRY_PPID:-}" ]] || continue
      case "$commrest" in
        *team-agent*) alive_ta="$pid"; RETRY_PID="$pid"; break ;;
      esac
    done < <(ps -ax -o pid,ppid,etime,stat,comm)
  fi
fi
if [[ -n "$alive_ta" ]]; then
  say "GREEN: second start still running without can't find window (launcher pid $alive_ta)"
  exit 0
fi
say "RETRY_PANE_PID=${RETRY_PANE_PID:-empty} RETRY_PPID=${RETRY_PPID:-empty} alive_ta=${alive_ta:-empty}"
if [[ -n "${WS_SOCK:-}" ]] && ! protected "$WS_SOCK"; then
  retry_wins="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -F '#{window_name}' 2>/dev/null || true)"
  say "RETRY_WINDOWS=${retry_wins:-empty}"
  if printf '%s\n' "$retry_wins" | grep -qx 'claude_code'; then
    say "GREEN: second start created claude_code window without can't find window"
    exit 0
  fi
fi
die2 "second start produced neither refusal nor RC nor live launcher"
