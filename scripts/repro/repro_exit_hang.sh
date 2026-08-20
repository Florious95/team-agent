#!/usr/bin/env bash
# purpose: Q2 复现装置——Ctrl-C×2 后 launcher 不落回 shell
# contract: exit 1=红=复现成功(N 秒内记录的 launcher pid 仍在);
#           exit 0=绿=已落回; exit 2=不可判。自清理本装置创建的 socket/目录。
# boundary: 只在 mktemp 隔离目录起 team-agent claude; 禁裸 tmux; 禁碰保护 socket;
#           ps 窄字段; 只杀本装置记录的精确 PID。
#
# 预算: 首启最多 75s + hang 观察 10s + 清理。高载下超时进 2,不进 1。

set -u

DEFAULT_GAUGE="/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent"
DEFAULT_MD5="2b7cf51937ea2d50897eeba75fae3b6b"
GAUGE="${REPRO_GAUGE:-$DEFAULT_GAUGE}"
EXPECT_MD5="${REPRO_GAUGE_MD5:-}"
if [[ -z "${REPRO_GAUGE:-}" ]]; then
  EXPECT_MD5="$DEFAULT_MD5"
fi
HANG_SECS="${REPRO_HANG_SECS:-10}"
FIRST_WAIT="${REPRO_FIRST_WAIT:-75}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
export PATH="/Users/alauda/ta-tmux-shim/bin:/opt/homebrew/bin:/Users/alauda/.local/bin:/usr/bin:/bin:${PATH:-}"
unset TMUX TMUX_PANE

TMUX_BIN="$(command -v tmux || true)"
tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

CTRL=""
WS=""
LAUNCHER_PID=""
WS_SOCK=""
CLEANED=0
VERDICT=2
REASON="unset"

say() { printf '%s\n' "$*"; }
protected() { [[ "$PROTECTED" == *" $1 "* ]]; }

# kill-server 后 macOS 可能留 stale socket 文件。只删本装置记下的名字,且先确认无 server。
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

die2() { REASON="$*"; say "UNJUDGEABLE: $REASON"; VERDICT=2; exit 2; }

cleanup() {
  local rc=$?
  [[ "$CLEANED" -eq 1 ]] && return
  CLEANED=1
  if [[ -n "${LAUNCHER_PID:-}" ]]; then
    if ps -p "$LAUNCHER_PID" -o pid= >/dev/null 2>&1; then
      kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
      sleep 1
      if ps -p "$LAUNCHER_PID" -o pid= >/dev/null 2>&1; then
        kill -KILL "$LAUNCHER_PID" 2>/dev/null || true
      fi
    fi
  fi
  rm_stale_sock "${WS_SOCK:-}"
  rm_stale_sock "${CTRL:-}"
  if [[ -n "${WS:-}" ]]; then
    case "$WS" in
      /tmp/ta-t103-*|/private/tmp/ta-t103-*) rm -rf "$WS" ;;
    esac
  fi
  return 0
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
MTIME="$(stat -f '%Sm' "$GAUGE")"
SIZE="$(stat -f '%z' "$GAUGE")"
VER="$("$GAUGE" --version 2>/dev/null || true)"
say "GAUGE path=$GAUGE md5=$GOT_MD5 mtime=$MTIME size=$SIZE version=$VER"
say "HANG_SECS=$HANG_SECS FIRST_WAIT=$FIRST_WAIT"

WS="$(mktemp -d /tmp/ta-t103-XXXXXX)"
git -C "$WS" init -q
say "WS=$WS"

CTRL="t103h$$"
if protected "$CTRL"; then
  die2 "ctrl socket name collided with protected: $CTRL"
fi
"$TMUX_BIN" -L "$CTRL" new-session -d -s ctrl -n launch -x 140 -y 40 \
  "unset TMUX TMUX_PANE; cd '$WS' || exit 91; exec '$GAUGE' claude"
sleep 0.4
PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:launch '#{pane_pid}' 2>/dev/null || true)"
if [[ -z "$PANE_PID" ]]; then
  die2 "control pane pid empty"
fi
say "CTRL=$CTRL PANE_PID=$PANE_PID"

# 等到 pane 上的进程是 team-agent(exec 后 pane_pid 即 launcher)
t=0
LAUNCHER_PID=""
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  comm="$(ps -p "$PANE_PID" -o comm= 2>/dev/null || true)"
  case "$comm" in
    *team-agent*) LAUNCHER_PID="$PANE_PID"; break ;;
  esac
  # 若还没 exec, 找 pane 的子进程
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
if [[ -z "$LAUNCHER_PID" ]]; then
  cap="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:launch -S -40 2>/dev/null || true)"
  say "FIRST_CAPTURE_ON_FAIL:"
  say "$cap"
  die2 "launcher pid not found within ${FIRST_WAIT}s (pane comm=$(ps -p "$PANE_PID" -o comm= 2>/dev/null || echo none))"
fi
say "LAUNCHER_PID=$LAUNCHER_PID comm=$(ps -p "$LAUNCHER_PID" -o comm=)"

# 等到 attach 子进程 + workspace socket/window, 否则 C-c 可能打在尚未 attach 的 launcher 上
ATTACH_PID=""
t=0
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
p = sys.argv[1]
with open(p) as f:
    d = json.load(f)
if "tmux_socket" not in d:
    sys.exit(3)
from os.path import basename
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

say "ATTACH_PID=${ATTACH_PID:-empty} WS_SOCK=${WS_SOCK:-empty}"
if [[ -z "$ATTACH_PID" ]]; then
  cap="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:launch -S -60 2>/dev/null || true)"
  say "FIRST_CAPTURE_NO_ATTACH:"
  say "$cap"
  die2 "attach child not observed; C-c would not be the user-facing path"
fi
if [[ -z "$WS_SOCK" ]]; then
  die2 "workspace tmux_socket never appeared in state.json"
fi
ps -p "$LAUNCHER_PID" -o pid,ppid,etime,stat,comm
ps -p "$ATTACH_PID" -o pid,ppid,etime,stat,comm

say "SEND_CC_START=$(date '+%H:%M:%S')"
"$TMUX_BIN" -L "$CTRL" send-keys -t ctrl:launch C-c
sleep 0.4
"$TMUX_BIN" -L "$CTRL" send-keys -t ctrl:launch C-c
say "SEND_CC_DONE=$(date '+%H:%M:%S') observing ${HANG_SECS}s"

sleep "$HANG_SECS"

say "PS_AFTER_CC:"
ps -p "$LAUNCHER_PID" -o pid,stat
ps_rc=$?
CAP_AFTER="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:launch -S -30 2>/dev/null || true)"
say "CAPTURE_AFTER_CC:"
say "$CAP_AFTER"

if [[ "$ps_rc" -eq 0 ]]; then
  # 进程还在 = 未落回 shell。画面里出现 ^C 不算退出。
  if printf '%s\n' "$CAP_AFTER" | grep -q 'LAUNCHER_RC='; then
    say "GREEN: launcher pid still listed but capture has LAUNCHER_RC — treating as returned"
    VERDICT=0
    REASON="capture shows LAUNCHER_RC while pid table is racy"
    exit 0
  fi
  say "RED: launcher pid $LAUNCHER_PID still alive after C-c x2 + ${HANG_SECS}s"
  VERDICT=1
  exit 1
fi

say "GREEN: launcher pid $LAUNCHER_PID gone after C-c x2 + ${HANG_SECS}s"
VERDICT=0
exit 0
