#!/usr/bin/env bash
# purpose: t.p9 复现装置——`team-agent grok`/`cursor` 当头后，该目录 socket 下出现对应 leader pane
# contract: exit 1=红=子命令不存在(invalid choice)或启动后无对应窗;
#           exit 0=绿=同一 workspace socket 上同时有窗 `grok` 与 `cursor_agent`;
#           exit 2=不可判。自清理本装置 socket/目录。
# boundary: 只在 mktemp 隔离目录; 禁裸 tmux; 禁碰保护 socket; ps 窄字段;
#           只杀本装置记录的精确 PID。PATH 前置本装置 fake grok/agent/claude。
#
# 基线 b81c70816ff504d44f1d4a041373c84f 必红: argparse invalid choice。
# 新件绿: passthrough 起 managed 窗。不把 worker 三拍/`--resume` 写进 argv。

set -u

GAUGE="${REPRO_GAUGE:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="${REPRO_GAUGE_MD5:-}"
FIRST_WAIT="${REPRO_FIRST_WAIT:-75}"
SECOND_WAIT="${REPRO_SECOND_WAIT:-40}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
export PATH="/Users/alauda/ta-tmux-shim/bin:/opt/homebrew/bin:/Users/alauda/.local/bin:/usr/bin:/bin:${PATH:-}"
unset TMUX TMUX_PANE

TMUX_BIN="$(command -v tmux || true)"
tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

CTRL=""
WS=""
FAKEBIN=""
G_PID=""
C_PID=""
G_ATTACH=""
C_ATTACH=""
WS_SOCK=""
WS_SOCK2=""
PS_SNAP=""
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
  for pid in "${C_PID:-}" "${C_ATTACH:-}" "${G_PID:-}" "${G_ATTACH:-}"; do
    kill_recorded "$pid"
  done
  sleep 1
  for pid in "${C_PID:-}" "${C_ATTACH:-}" "${G_PID:-}" "${G_ATTACH:-}"; do
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
      /tmp/ta-p9-*|/private/tmp/ta-p9-*) rm -rf "$WS" ;;
    esac
  fi
  if [[ -n "${PS_SNAP:-}" ]]; then
    case "$PS_SNAP" in
      /tmp/ta-p9-ps-*|/private/tmp/ta-p9-ps-*) rm -f "$PS_SNAP" ;;
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
  local comm pid ppid _etime _stat commrest
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

HELP="$("$GAUGE" --help 2>&1 || true)"
say "=== --help Provider launchers excerpt ==="
printf '%s\n' "$HELP" | grep -A1 -F "Provider launchers" || true

# Direct argv tooth: `-h` does not spawn a leader. Baseline invalid-choice
# is the red for "subcommand missing". Isolation cwd so logs cannot land on
# the caller workspace.
HELP_WS="$(mktemp -d /tmp/ta-p9-help-XXXXXX)"
GROK_DIRECT_ERR="$(mktemp /tmp/ta-p9-err-XXXXXX)"
CURSOR_DIRECT_ERR="$(mktemp /tmp/ta-p9-err-XXXXXX)"
set +e
(cd "$HELP_WS" && "$GAUGE" grok -h >/dev/null 2>"$GROK_DIRECT_ERR")
GROK_DIRECT_RC=$?
(cd "$HELP_WS" && "$GAUGE" cursor -h >/dev/null 2>"$CURSOR_DIRECT_ERR")
CURSOR_DIRECT_RC=$?
set -e
rm -rf "$HELP_WS"
say "GROK_DIRECT_RC=$GROK_DIRECT_RC"
say "GROK_DIRECT_ERR=$(tr '\n' ' ' < "$GROK_DIRECT_ERR")"
say "CURSOR_DIRECT_RC=$CURSOR_DIRECT_RC"
say "CURSOR_DIRECT_ERR=$(tr '\n' ' ' < "$CURSOR_DIRECT_ERR")"
GROK_INVALID=0
CURSOR_INVALID=0
if grep -q "invalid choice: 'grok'" "$GROK_DIRECT_ERR"; then
  GROK_INVALID=1
fi
if grep -q "invalid choice: 'cursor'" "$CURSOR_DIRECT_ERR"; then
  CURSOR_INVALID=1
fi
say "GROK_INVALID_CHOICE=$GROK_INVALID CURSOR_INVALID_CHOICE=$CURSOR_INVALID"
rm -f "$GROK_DIRECT_ERR" "$CURSOR_DIRECT_ERR"

if [[ "$GROK_INVALID" -eq 1 || "$CURSOR_INVALID" -eq 1 ]]; then
  say "RED: subcommand missing (invalid choice) grok=$GROK_INVALID cursor=$CURSOR_INVALID"
  exit 1
fi

WS="$(mktemp -d /tmp/ta-p9-XXXXXX)"
git -C "$WS" init -q
say "WS=$WS"
PS_SNAP="$(mktemp /tmp/ta-p9-ps-XXXXXX)"
FAKEBIN="$WS/.fake-bin"
mkdir -p "$FAKEBIN"
for cmd in grok agent claude; do
  cat > "$FAKEBIN/$cmd" <<'SH'
#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s 0.0.0-repro\n' "$(basename "$0")"
  exit 0
fi
while :; do sleep 1; done
SH
  chmod +x "$FAKEBIN/$cmd"
done

CTRL="tp9h$$"
if protected "$CTRL"; then
  die2 "ctrl socket name collided with protected: $CTRL"
fi

start_head() {
  local win="$1"
  local verb="$2"
  "$TMUX_BIN" -L "$CTRL" new-window -d -n "$win" \
    "unset TMUX TMUX_PANE; export PATH='$FAKEBIN:$PATH'; cd '$WS' || exit 91; exec '$GAUGE' $verb"
}

"$TMUX_BIN" -L "$CTRL" new-session -d -s ctrl -n grok -x 140 -y 40 \
  "unset TMUX TMUX_PANE; export PATH='$FAKEBIN:$PATH'; cd '$WS' || exit 91; exec '$GAUGE' grok"
sleep 0.4
G_PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:grok '#{pane_pid}' 2>/dev/null || true)"
[[ -n "$G_PANE_PID" ]] || die2 "control grok pane pid empty"
say "CTRL=$CTRL G_PANE_PID=$G_PANE_PID"

t=0
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  G_PID="$(find_ta_under "$G_PANE_PID" || true)"
  [[ -n "$G_PID" ]] && break
  cap="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:grok -S -40 2>/dev/null || true)"
  if printf '%s\n' "$cap" | grep -q "invalid choice: 'grok'"; then
    say "RED: grok invalid choice in pane"
    say "$cap"
    exit 1
  fi
  sleep 1
  t=$((t + 1))
done
[[ -n "$G_PID" ]] || die2 "grok launcher pid not found within ${FIRST_WAIT}s"

t=0
while [[ "$t" -lt "$FIRST_WAIT" ]]; do
  if [[ -z "$G_ATTACH" ]]; then
    G_ATTACH="$(find_tmux_child "$G_PID" || true)"
  fi
  if [[ -f "$WS/.team/runtime/state.json" && -z "$WS_SOCK" ]]; then
    WS_SOCK="$(read_sock_from_state "$WS/.team/runtime/state.json" 2>/dev/null || true)"
  fi
  if [[ -n "$WS_SOCK" ]] && protected "$WS_SOCK"; then
    die2 "workspace socket is protected: $WS_SOCK — refusing to touch"
  fi
  if [[ -n "$G_ATTACH" && -n "$WS_SOCK" ]]; then
    wins="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -a -F '#{window_name}' 2>/dev/null || true)"
    if printf '%s\n' "$wins" | grep -qx 'grok'; then
      break
    fi
  fi
  cap="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:grok -S -40 2>/dev/null || true)"
  if printf '%s\n' "$cap" | grep -q "not found"; then
    say "RED: grok command not found (loud missing CLI) cap=$cap"
    exit 1
  fi
  sleep 1
  t=$((t + 1))
done
say "G_PID=$G_PID G_ATTACH=${G_ATTACH:-empty} WS_SOCK=${WS_SOCK:-empty} waited=${t}s"
[[ -n "$WS_SOCK" ]] || { say "RED: workspace socket never appeared after grok"; exit 1; }
wins_g="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -a -F '#{window_name}' 2>/dev/null || true)"
say "WINDOWS_AFTER_GROK=$wins_g"
if ! printf '%s\n' "$wins_g" | grep -qx 'grok'; then
  say "RED: no grok window on workspace socket"
  exit 1
fi

start_head cursor cursor
sleep 0.4
C_PANE_PID="$("$TMUX_BIN" -L "$CTRL" display-message -p -t ctrl:cursor '#{pane_pid}' 2>/dev/null || true)"
[[ -n "$C_PANE_PID" ]] || die2 "control cursor pane pid empty"

t=0
while [[ "$t" -lt "$SECOND_WAIT" ]]; do
  C_PID="$(find_ta_under "$C_PANE_PID" || true)"
  if [[ -n "$C_PID" && -z "$C_ATTACH" ]]; then
    C_ATTACH="$(find_tmux_child "$C_PID" || true)"
  fi
  if [[ -f "$WS/.team/runtime/state.json" ]]; then
    WS_SOCK2="$(read_sock_from_state "$WS/.team/runtime/state.json" 2>/dev/null || true)"
  fi
  if [[ -n "$WS_SOCK" ]]; then
    wins="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -a -F '#{window_name}' 2>/dev/null || true)"
    if printf '%s\n' "$wins" | grep -qx 'cursor_agent'; then
      break
    fi
  fi
  cap="$("$TMUX_BIN" -L "$CTRL" capture-pane -p -t ctrl:cursor -S -40 2>/dev/null || true)"
  if printf '%s\n' "$cap" | grep -q "invalid choice: 'cursor'"; then
    say "RED: cursor invalid choice in pane"
    say "$cap"
    exit 1
  fi
  sleep 1
  t=$((t + 1))
done
say "C_PID=${C_PID:-empty} C_ATTACH=${C_ATTACH:-empty} WS_SOCK2=${WS_SOCK2:-empty} waited=${t}s"
say "=== sessions ==="
"$TMUX_BIN" -L "$WS_SOCK" list-sessions 2>&1 || true
say "=== windows -a ==="
"$TMUX_BIN" -L "$WS_SOCK" list-windows -a 2>&1 || true
say "=== panes ==="
"$TMUX_BIN" -L "$WS_SOCK" list-panes -a -F 'pane=#{pane_id} win=#{window_name} sess=#{session_name}' 2>&1 || true

if [[ -n "$WS_SOCK2" && "$WS_SOCK2" != "$WS_SOCK" ]]; then
  say "RED: cursor used a different socket $WS_SOCK2 vs grok $WS_SOCK"
  exit 1
fi
wins_c="$("$TMUX_BIN" -L "$WS_SOCK" list-windows -a -F '#{window_name}' 2>/dev/null || true)"
say "WINDOWS_AFTER_CURSOR=$wins_c"
if ! printf '%s\n' "$wins_c" | grep -qx 'cursor_agent'; then
  say "RED: no cursor_agent window on workspace socket"
  exit 1
fi
if ! printf '%s\n' "$wins_c" | grep -qx 'grok'; then
  say "RED: grok window gone after cursor start"
  exit 1
fi

say "GREEN: same socket $WS_SOCK has windows grok and cursor_agent"
exit 0
