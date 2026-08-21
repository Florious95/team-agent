#!/usr/bin/env bash
# //!
# //! purpose: 构造性接收态装置——隔离 tmux + fake-worker，经 team-agent send
# //!          走框架注入。①主动 choose-tree 后注入（tree-mode，非用户样本复现）；
# //!          ②发前必须字面闭合 CSI 201~（括号未闭合接收态）。修前红、修后绿。
# //! contract:
# //!   provides:
# //!     - name: treemode-eats-enter-repro
# //!       what: 一条命令，退出码即判据；1=Enter 前无 q（tree）或无 CSI 201~；
# //!             0=q+201~ 都在第一次 Enter 前且假工人消费了 token；2=不可判
# //!   requires:
# //!     - name: team-agent-gauge
# //!       what: 默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!             md5 b81c70816ff504d44f1d4a041373c84f；TEAM_AGENT_BIN 可覆盖
# //! boundary:
# //!   - 三态：0 需求已满足 / 1 复现 / 2 不可判；禁止把 2 折进 1
# //!   - 隔离 workspace + 私有 socket；不继承 raw TMUX/TMUX_PANE
# //!   - qs 到达看 all_workers_spawned，不看 rc=0
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的精确 PID
# //!   - 不造假红：注入臂未对目标 pane 发出 Enter/C-m 不得报 1
# //!   - 不造假绿：仅凭重试第二次 Enter 把树关掉后提交不算绿
# //!   - E55：日志里出现 Escape/C-c 当注入手段 → 2（契约禁止，不是本缺陷）
# //!   - 预算：单次 180s；超时 exit 2
# //! maturity: wired
#
# 用法: bash scripts/repro/repro_treemode_eats_enter.sh
# 环境: TEAM_AGENT_BIN  REPRO_GAUGE_MD5  KEEP_TMP=1  BUDGET_SEC

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="${REPRO_GAUGE_MD5:-b81c70816ff504d44f1d4a041373c84f}"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-d123-treemode-$$"
TEAM_ID="tm18$$"
AGENT_ID="w1"
NORMAL_TOK="TM18N-$$"
TREE_TOK="TM18T-$$"
WS="$RUN/ws"
TEAMDIR="$WS/team"
BINDIR="$RUN/bin"
EVID="$NODE/runs/$(date +%Y%m%dT%H%M%S)-$$"
BUDGET_SEC="${BUDGET_SEC:-180}"
PROTECTED=" ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf "
PIDS=""
SOCK=""
OUTER_SOCK=""
STARTED=$(date +%s)
CLEANED=0
TA=""
BIN_MD5=""
TMUX_BIN=""
COORD_PID=""
W1_PANE=""
SEND2_MID=""

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

budget_hit() {
  NOW=$(date +%s)
  [ $((NOW - STARTED)) -ge "$BUDGET_SEC" ]
}

say() { printf '%s\n' "$*"; }

finish() {
  _rc=$1
  NOW=$(date +%s)
  ELAP=$((NOW - STARTED))
  say "RESULT rc=$_rc elapsed=${ELAP}s load_before=${LOAD_BEFORE:-} load_after=$(load1) bin=$TA md5=$BIN_MD5"
  if [ "$CLEANED" -eq 0 ]; then
    CLEANED=1
    if [ -n "$TA" ] && [ -n "$WS" ] && [ -x "$TA" ]; then
      "$TA" shutdown --workspace "$WS" --team "$TEAM_ID" --json >/dev/null 2>&1 || true
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
    if [ -n "$OUTER_SOCK" ] && [ -n "$TMUX_BIN" ]; then
      _ob=$(basename "$OUTER_SOCK")
      if ! protected_name "$_ob"; then
        "$TMUX_BIN" -S "$OUTER_SOCK" kill-server >/dev/null 2>&1 || true
      fi
    fi
    if [ -n "$SOCK" ] && [ -n "$TMUX_BIN" ]; then
      if ! protected_name "$SOCK"; then
        "$TMUX_BIN" -L "$SOCK" kill-server >/dev/null 2>&1 || true
      fi
    fi
    if [ "${KEEP_TMP:-0}" != "1" ] && [ -n "$RUN" ]; then
      case "$RUN" in
        *ta-d123-treemode-*) rm -rf "$RUN" ;;
      esac
    fi
  fi
  exit "$_rc"
}

trap 'unjudgeable interrupted' INT TERM

LOAD_BEFORE=$(load1)
NCPU=$(/usr/sbin/sysctl -n hw.ncpu 2>/dev/null || echo 0)
say "LOAD before=$LOAD_BEFORE ncpu=$NCPU"

# Strip ambient team-agent / tmux so this device cannot target the live team.
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

if [ ! -x "$SRC_BIN" ]; then
  unjudgeable "gauge missing: $SRC_BIN"
fi
if [ ! -x "$REAL_TMUX" ]; then
  if command -v tmux >/dev/null 2>&1; then
    REAL_TMUX=$(command -v tmux)
  else
    unjudgeable "tmux not found"
  fi
fi
TMUX_BIN="$REAL_TMUX"

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$BINDIR" "$EVID" || unjudgeable "mkdir failed"
TA="$BINDIR/team-agent"
cp "$SRC_BIN" "$TA" || unjudgeable "cp gauge failed"
chmod +x "$TA"
BIN_MD5=$(md5 -q "$TA" 2>/dev/null || md5sum "$TA" | awk '{print $1}')
BIN_MTIME=$(stat -f '%Sm' "$TA" 2>/dev/null || stat -c '%y' "$TA" 2>/dev/null || echo unknown)
BIN_SIZE=$(stat -f '%z' "$TA" 2>/dev/null || stat -c '%s' "$TA" 2>/dev/null || echo unknown)
say "GAUGE path=$TA md5=$BIN_MD5 mtime=$BIN_MTIME size=$BIN_SIZE version=$("$TA" --version 2>/dev/null || true)"
if [ -n "$EXPECT_MD5" ] && [ "$BIN_MD5" != "$EXPECT_MD5" ]; then
  say "NOTE: md5 $BIN_MD5 != default baseline $EXPECT_MD5 (candidate run or REPRO_GAUGE_MD5 override)"
fi

# tmux wrapper: log every argv then exec the real binary. Isolates this device.
cat > "$BINDIR/tmux" <<'WRAP'
#!/bin/sh
log=${TA_TMUX_LOG:-/dev/null}
real=${TA_REAL_TMUX:-/opt/homebrew/bin/tmux}
{
  printf 'TMUX'
  for a in "$@"; do
    printf ' %s' "$a"
  done
  printf '\n'
} >> "$log"
exec "$real" "$@"
WRAP
chmod +x "$BINDIR/tmux"
export TA_TMUX_LOG="$RUN/tmux.log"
export TA_REAL_TMUX="$REAL_TMUX"
export PATH="$BINDIR:/opt/homebrew/bin:/usr/bin:/bin"
tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

cat > "$TEAMDIR/TEAM.md" <<EOF
---
name: $TEAM_ID
objective: PR-18 tree-mode inject probe.
provider: fake
display_backend: none
---

Probe team.
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

Wait for instructions.
EOF

say "WS=$WS TEAM=$TEAM_ID"
"$TA" quick-start "$TEAMDIR" --workspace "$WS" --yes --no-display --json >"$RUN/qs.json" 2>"$RUN/qs.err" || true
copy_evid() { [ -e "$1" ] && cp "$1" "$EVID/" 2>/dev/null || true; }
copy_evid "$RUN/qs.json"
copy_evid "$RUN/qs.err"

python3 - "$RUN/qs.json" <<'PY' >"$RUN/qs.parsed" || unjudgeable "qs json parse failed"
import json, sys
p = sys.argv[1]
try:
    d = json.load(open(p))
except Exception as e:
    print("parse_error", e)
    raise SystemExit(1)
wr = d.get("worker_readiness") or d.get("readiness") or {}
spawned = wr.get("all_workers_spawned")
print("ok", d.get("ok"))
print("all_workers_spawned", spawned)
print("reason", wr.get("reason") or d.get("reason"))
if spawned is not True:
    raise SystemExit(2)
PY
qs_rc=$?
cat "$RUN/qs.parsed"
if [ "$qs_rc" -ne 0 ]; then
  unjudgeable "quick-start did not spawn workers (all_workers_spawned != true); see $RUN/qs.json"
fi

if [ -f "$WS/.team/runtime/coordinator.pid" ]; then
  COORD_PID=$(tr -d ' \n' < "$WS/.team/runtime/coordinator.pid")
  record_pid "$COORD_PID"
  say "COORD_PID=$COORD_PID"
fi

SOCK=$(python3 - "$WS/.team/runtime/state.json" <<'PY'
import json, sys, os
d = json.load(open(sys.argv[1]))
s = d.get("tmux_socket") or ""
print(os.path.basename(s))
PY
) || unjudgeable "state.json missing tmux_socket"
say "SOCK=$SOCK"
if [ -z "$SOCK" ]; then
  unjudgeable "empty tmux_socket"
fi
if protected_name "$SOCK"; then
  unjudgeable "workspace socket is protected: $SOCK — refusing to touch"
fi

# Resolve w1 pane id from live tmux (not from possibly-stale state).
t=0
W1_PANE=""
while [ "$t" -lt 30 ]; do
  budget_hit && unjudgeable "budget while waiting w1 pane"
  W1_PANE=$("$TMUX_BIN" -L "$SOCK" list-windows -a -F '#{window_name} #{pane_id}' 2>/dev/null | awk -v a="$AGENT_ID" '$1==a{print $2; exit}')
  [ -n "$W1_PANE" ] && break
  sleep 1
  t=$((t + 1))
done
[ -n "$W1_PANE" ] || unjudgeable "w1 pane not found"
say "W1_PANE=$W1_PANE"

t=0
while [ "$t" -lt 30 ]; do
  budget_hit && unjudgeable "budget while waiting FAKE_READY"
  cap=$("$TMUX_BIN" -L "$SOCK" capture-pane -p -t "$W1_PANE" 2>/dev/null || true)
  case "$cap" in
    *TEAM_AGENT_FAKE_READY*) break ;;
  esac
  sleep 1
  t=$((t + 1))
done
case "$cap" in
  *TEAM_AGENT_FAKE_READY*) say "FAKE_READY after ${t}s" ;;
  *) unjudgeable "fake worker never printed TEAM_AGENT_FAKE_READY" ;;
esac

wait_token() {
  _tok=$1
  _secs=$2
  _i=0
  while [ "$_i" -lt "$_secs" ]; do
    budget_hit && return 2
    _cap=$("$TMUX_BIN" -L "$SOCK" capture-pane -p -t "$W1_PANE" 2>/dev/null || true)
    case "$_cap" in
      *"$_tok"*) echo "$_cap"; return 0 ;;
      *TEAM_AGENT_FAKE_WORKING*) echo "$_cap"; return 0 ;;
    esac
    sleep 1
    _i=$((_i + 1))
  done
  echo "$_cap"
  return 1
}

# Positive control: framework inject without tree-mode must reach the fake worker.
say "SEND1 $NORMAL_TOK"
"$TA" send --workspace "$WS" --team "$TEAM_ID" "$AGENT_ID" "positive-control $NORMAL_TOK" --json >"$RUN/send1.json" 2>"$RUN/send1.err" || unjudgeable "send1 failed"
copy_evid "$RUN/send1.json"
if ! wait_token "$NORMAL_TOK" 25 >/dev/null; then
  unjudgeable "positive control: inject never reached fake worker (token $NORMAL_TOK absent). wrapper/log=$TA_TMUX_LOG"
fi
say "POSITIVE_OK token=$NORMAL_TOK"

# Wait until worker returns to READY so tree-mode send is a distinct turn.
t=0
while [ "$t" -lt 20 ]; do
  cap=$("$TMUX_BIN" -L "$SOCK" capture-pane -p -t "$W1_PANE" 2>/dev/null || true)
  case "$cap" in
    *TEAM_AGENT_FAKE_READY*) break ;;
  esac
  sleep 1
  t=$((t + 1))
done

# Attached dummy client: tree-mode key dispatch needs a client (q works attached).
OUTER_SOCK="$RUN/outer.sock"
"$TMUX_BIN" -S "$OUTER_SOCK" new-session -d -s wrap -x 80 -y 24 \
  "$TMUX_BIN -L $SOCK attach-session -t team-${TEAM_ID}:${AGENT_ID}" \
  || unjudgeable "outer attach session failed"
sleep 0.8
OUTER_PANE_PID=$("$TMUX_BIN" -S "$OUTER_SOCK" display-message -p -t wrap '#{pane_pid}' 2>/dev/null || true)
record_pid "$OUTER_PANE_PID"
CLIENTS=$("$TMUX_BIN" -L "$SOCK" list-clients 2>/dev/null || true)
say "CLIENTS=$CLIENTS"
[ -n "$CLIENTS" ] || unjudgeable "no attached client on inner socket; tree-mode q would be unreachable"

# choose-tree is a client overlay; try pane target then untargeted. Poll — high load can delay.
t=0
MODE=""
INMODE=""
while [ "$t" -lt 10 ]; do
  "$TMUX_BIN" -L "$SOCK" choose-tree -t "$W1_PANE" >/dev/null 2>&1 || true
  sleep 0.4
  MODE=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1_PANE" '#{pane_mode}' 2>/dev/null || true)
  INMODE=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1_PANE" '#{pane_in_mode}' 2>/dev/null || true)
  [ "$MODE" = "tree-mode" ] && [ "$INMODE" = "1" ] && break
  "$TMUX_BIN" -L "$SOCK" choose-tree >/dev/null 2>&1 || true
  sleep 0.4
  MODE=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1_PANE" '#{pane_mode}' 2>/dev/null || true)
  INMODE=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1_PANE" '#{pane_in_mode}' 2>/dev/null || true)
  [ "$MODE" = "tree-mode" ] && [ "$INMODE" = "1" ] && break
  t=$((t + 1))
done
say "TREE_PRECONDITION pane_mode=[$MODE] pane_in_mode=[$INMODE] tries=$t"
[ "$MODE" = "tree-mode" ] || unjudgeable "choose-tree did not enter tree-mode (pane_mode=[$MODE])"
[ "$INMODE" = "1" ] || unjudgeable "pane_in_mode != 1 after choose-tree"

# Snapshot tmux log length so we only judge inject after this point.
LOG_MARK=$(wc -l < "$TA_TMUX_LOG" | tr -d ' ')

say "SEND2 $TREE_TOK"
"$TA" send --workspace "$WS" --team "$TEAM_ID" "$AGENT_ID" "tree-mode-probe $TREE_TOK" --json >"$RUN/send2.json" 2>"$RUN/send2.err" || unjudgeable "send2 failed"
copy_evid "$RUN/send2.json"
SEND2_MID=$(python3 - "$RUN/send2.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
print(d.get("message_id") or "")
PY
)
say "SEND2_MID=$SEND2_MID"

# Wait until the wrapper sees a submit key to the worker pane (inject arm ran).
t=0
INJECT_SEEN=0
while [ "$t" -lt 30 ]; do
  budget_hit && unjudgeable "budget waiting inject argv"
  if python3 - "$TA_TMUX_LOG" "$LOG_MARK" "$W1_PANE" <<'PY'
import sys
path, mark, pane = sys.argv[1], int(sys.argv[2]), sys.argv[3]
lines = open(path).read().splitlines()
tail = lines[mark:]
for ln in tail:
    if "send-keys" in ln and pane in ln and (" C-m" in ln or " Enter" in ln):
        sys.exit(0)
sys.exit(1)
PY
  then
    INJECT_SEEN=1
    break
  fi
  sleep 1
  t=$((t + 1))
done
[ "$INJECT_SEEN" = "1" ] || unjudgeable "inject arm never sent Enter/C-m to $W1_PANE after tree-mode send (no tooth)"

python3 - "$TA_TMUX_LOG" "$LOG_MARK" "$W1_PANE" <<'PY' >"$RUN/inject_argv.txt"
import sys
path, mark, pane = sys.argv[1], int(sys.argv[2]), sys.argv[3]
lines = open(path).read().splitlines()
tail = lines[mark:]
print("TAIL_LINES", len(tail))
for ln in tail:
    if "send-keys" in ln and pane in ln:
        print(ln)
PY
copy_evid "$RUN/inject_argv.txt"
copy_evid "$TA_TMUX_LOG"
cat "$RUN/inject_argv.txt"

# E55: Escape / Ctrl-C must not appear as inject keys.
if python3 - "$RUN/inject_argv.txt" <<'PY'
import sys
text = open(sys.argv[1]).read()
bad = False
for tok in (" Escape", " C-c", " C-C"):
    if tok in text:
        bad = True
sys.exit(0 if bad else 1)
PY
then
  unjudgeable "E55 violation: Escape or C-c appeared in inject send-keys (not a tree-mode miss)"
fi

# Red/green: first submit Enter after tree-mode must be preceded by send-keys q on same pane.
python3 - "$TA_TMUX_LOG" "$LOG_MARK" "$W1_PANE" <<'PY' >"$RUN/verdict_argv.txt"
import sys
path, mark, pane = sys.argv[1], int(sys.argv[2]), sys.argv[3]
lines = open(path).read().splitlines()
tail = lines[mark:]
first_enter = None
for i, ln in enumerate(tail):
    if "send-keys" in ln and pane in ln and (" C-m" in ln or " Enter" in ln):
        first_enter = i
        break
if first_enter is None:
    print("first_enter=missing")
    raise SystemExit(2)
# Walk backwards for cancel on same pane before this Enter.
q = False
xcancel = False
for ln in reversed(tail[:first_enter]):
    if "send-keys" not in ln or pane not in ln:
        continue
    parts = ln.split()
    if parts[-1] == "q" or (len(parts) >= 2 and parts[-1] == "q"):
        q = True
        break
    if "-X" in parts and "cancel" in parts:
        xcancel = True
        break
    # stop at previous submit
    if "C-m" in parts or "Enter" in parts:
        break
csi201 = False
for ln in tail[:first_enter]:
    if "send-keys" in ln and pane in ln and "-l" in ln.split() and "[201~" in ln:
        csi201 = True
        break
print("first_enter_idx", first_enter)
print("cancel_q", q)
print("cancel_x", xcancel)
print("csi201", csi201)
print("first_enter_line", tail[first_enter])
if q and csi201:
    raise SystemExit(0)
raise SystemExit(1)
PY
argv_rc=$?
cat "$RUN/verdict_argv.txt"
copy_evid "$RUN/verdict_argv.txt"

MODE_AFTER=$("$TMUX_BIN" -L "$SOCK" display-message -p -t "$W1_PANE" '#{pane_mode}' 2>/dev/null || true)
say "TREE_AFTER pane_mode=[$MODE_AFTER]"

if [ "$argv_rc" -eq 2 ]; then
  unjudgeable "could not locate first Enter in tmux wrapper log"
fi

if [ "$argv_rc" -eq 0 ]; then
  if wait_token "$TREE_TOK" 20 >/dev/null; then
    pass_ok "prepare_pane_for_submit: q+CSI201 before Enter; fake worker consumed $TREE_TOK (constructive choose-tree + unclosed-paste close)"
  else
    unjudgeable "q+201~ sent before Enter but fake worker never consumed $TREE_TOK"
  fi
fi

fail_repro "inject sent Enter/C-m to $W1_PANE without prior tree cancel q and/or CSI 201~ (constructive choose-tree). after_mode=[$MODE_AFTER] log=$TA_TMUX_LOG"
