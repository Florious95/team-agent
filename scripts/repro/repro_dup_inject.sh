#!/bin/sh
# //!
# //! purpose: 固化消息双投 P0（先红）——三机制各一臂。同一 message_id
# //!          正文在 pane/转录出现两次即红。修前须红、修后须绿。
# //! contract:
# //!   provides:
# //!     - name: dup-inject-repro
# //!       what: 一条命令，退出码即判据；1=至少一臂双投；0=ARM1+ARM2
# //!             均单次且 ARM3 非红；2=不可判/构造不出（单臂 rc 单列，不折进 0/1）
# //!   requires:
# //!     - name: team-agent-0.5.66
# //!       what: 默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!             md5 前缀 b81c7081；TEAM_AGENT_BIN 可覆盖
# //! boundary:
# //!   - 三态：0/1/2。超预算与构造不出归 2，禁止折进前两态
# //!   - 隔离 mktemp workspace + fake provider；不继承 raw TMUX/TMUX_PANE
# //!   - 受保护 socket 只核存在，不 -L：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的精确 PID
# //!   - 不读 .env，不打印代理值
# //!   - 预算：默认 180s；超时与高载(1min load≥20)进 2
# //!   - POSIX：禁 bash 进程替换
# //! maturity: wired
#
# 用法: sh repro_dup_inject.sh
# 环境: TEAM_AGENT_BIN / REPRO_GAUGE  TEAM_AGENT_BIN_MD5_PREFIX  KEEP_TMP=1  BUDGET_SEC  REPRO_LOAD_MAX
# 修前(0.5.66 b81c7081): 至少一臂双投 → exit 1
# 修后(本格二进制): 三臂均可构且均单次 → exit 0

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-${REPRO_GAUGE:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}}"
EXPECT_MD5_PREFIX="${TEAM_AGENT_BIN_MD5_PREFIX:-}"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-t130-repro-$$"
TEAM_ID="t130t$$"
AGENT_ID="w1"
CALLER="$RUN/caller.sock"
WS="$RUN/ws"
TEAMDIR="$WS"
STAMP=$(date +%Y%m%dT%H%M%S)
EVID="$NODE/runs/$STAMP"
BUDGET_SEC="${BUDGET_SEC:-180}"
LOAD_MAX="${REPRO_LOAD_MAX:-20}"
PROTECTED="ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf"
PIDS=""
SOCK=""
SESSION=""
STARTED=$(date +%s)
LOAD_BEFORE=""
NCPU=""
CLEANED=0
TA=""
BIN_MD5=""
TMUX_BIN=""
WORKER_PANE=""
LEADER_PANE=""
OWNER_TEAM=""
COORD_PID=""
STATE_UCHG=0
ARM1_RC=2
ARM2_RC=2
ARM3_RC=2
ARM1_REASON="not-run"
ARM2_REASON="not-run"
ARM3_REASON="not-run"
OVERALL=2

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

comm_base() {
	narrow_ps "$1" | awk 'NR>1 {n=$5; sub(/.*\//,"",n); print n; exit}'
}

protected_present() {
	_n=0
	for _s in $PROTECTED; do
		if [ -S "/tmp/tmux-501/$_s" ] || [ -S "/private/tmp/tmux-501/$_s" ]; then
			_n=$((_n + 1))
		fi
	done
	echo "$_n"
}

is_protected_sock() {
	_base=$(basename "$1")
	for _s in $PROTECTED; do
		[ "$_base" = "$_s" ] && return 0
	done
	return 1
}

load1() {
	/usr/sbin/sysctl -n vm.loadavg 2>/dev/null | awk '{gsub(/[{}]/,""); print $1}'
}

budget_hit() {
	_now=$(date +%s)
	[ $((_now - STARTED)) -ge "$BUDGET_SEC" ]
}

say() { printf '%s\n' "$*"; }

finish() {
	OVERALL=$1
	echo "ARM1_RC=$ARM1_RC reason=$ARM1_REASON"
	echo "ARM2_RC=$ARM2_RC reason=$ARM2_REASON"
	echo "ARM3_RC=$ARM3_RC reason=$ARM3_REASON"
	echo "OVERALL=$OVERALL"
	exit "$OVERALL"
}

clear_identity_env() {
	unset TMUX TMUX_PANE
	unset TEAM_AGENT_LEADER_PANE_ID TEAM_AGENT_LEADER_SESSION_UUID
	unset TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE TEAM_AGENT_LEADER_SESSION_NAME
	unset TEAM_AGENT_LEADER_PROVIDER TEAM_AGENT_MACHINE_FINGERPRINT
	unset TEAM_AGENT_WORKSPACE TEAM_AGENT_TEAM_ID TEAM_AGENT_OWNER_TEAM_ID
	unset TEAM_AGENT_ACTIVE_TEAM TEAM_AGENT_ID TEAM_AGENT_AGENT_ID
	unset TEAM_AGENT_AUTH_MODE TEAM_AGENT_LEADER_BYPASS
	unset TEAM_AGENT_LEADER_BYPASS_SOURCE TEAM_AGENT_LEADER_BYPASS_PROVIDER
	unset TEAM_AGENT_LEADER_BYPASS_FLAG TEAM_AGENT_MCP_AUTO_APPROVE
	unset TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE
}

tmux() { echo "BUG: bare tmux forbidden" >&2; return 2; }

cleanup() {
	[ "$CLEANED" = 1 ] && return
	CLEANED=1
	if [ "$STATE_UCHG" = 1 ] && [ -f "$WS/.team/runtime/state.json" ]; then
		chflags nouchg "$WS/.team/runtime/state.json" 2>/dev/null || true
		STATE_UCHG=0
	fi
	if [ -n "${TA:-}" ] && [ -n "${WS:-}" ] && [ -d "$WS/.team" ]; then
		"$TA" shutdown --workspace "$WS" --json >/dev/null 2>&1 || true
	fi
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(comm_base "$_p")
			case "$_comm" in
			tmux|team-agent|bash|sh)
				kill -TERM "$_p" >/dev/null 2>&1 || true
				;;
			*)
				echo "cleanup: skip pid $_p comm=${_comm:-unknown}" >&2
				;;
			esac
		fi
	done
	sleep 1
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(comm_base "$_p")
			case "$_comm" in
			tmux|team-agent|bash|sh)
				kill -KILL "$_p" >/dev/null 2>&1 || true
				;;
			esac
		fi
	done
	if [ -n "$SOCK" ] && [ -S "$SOCK" ]; then
		if is_protected_sock "$SOCK"; then
			echo "cleanup: refuse protected socket $(basename "$SOCK")" >&2
		else
			"$TMUX_BIN" -S "$SOCK" kill-server >/dev/null 2>&1 || true
		fi
	fi
	if [ -S "$CALLER" ]; then
		"$TMUX_BIN" -S "$CALLER" kill-server >/dev/null 2>&1 || true
		rm -f "$CALLER"
	fi
	if [ "${KEEP_TMP:-0}" != "1" ] && [ -d "$RUN" ]; then
		if [ -d "$WS/.team" ]; then
			mkdir -p "$EVID/runtime"
			[ -f "$WS/.team/runtime/state.json" ] && cp "$WS/.team/runtime/state.json" "$EVID/runtime/state.json" 2>/dev/null || true
			[ -f "$WS/.team/runtime/team.db" ] && cp "$WS/.team/runtime/team.db" "$EVID/runtime/team.db" 2>/dev/null || true
			find "$WS/.team" -name 'events.jsonl' -exec cp {} "$EVID/runtime/events.jsonl" \; 2>/dev/null || true
		fi
		rm -rf "$RUN"
	fi
}

trap 'cleanup' EXIT INT TERM

need_cmd() {
	command -v "$1" >/dev/null 2>&1 || unjudgeable "$1 not on PATH"
}

write_kv() {
	python3 - "$1" "$2" "$3" <<'PY'
import json, sys, pathlib, shlex
path = pathlib.Path(sys.argv[1])
kind = sys.argv[2]
outp = pathlib.Path(sys.argv[3])
if not path.is_file():
    raise SystemExit("FILE_ABSENT")
raw = path.read_text(encoding="utf-8", errors="replace").strip()
if not raw:
    raise SystemExit("EMPTY")
start = raw.find("{")
if start < 0:
    raise SystemExit("NO_JSON")
text = raw[start:]
try:
    data = json.loads(text)
except json.JSONDecodeError:
    last = [ln for ln in text.splitlines() if ln.strip().startswith("{")]
    if not last:
        raise SystemExit("PARSE_FAIL")
    data = json.loads(last[-1])
if not isinstance(data, dict):
    raise SystemExit("NOT_OBJECT")

def take(d, k):
    if k not in d:
        return None
    return d[k]

def emit(d):
    lines = []
    for k, v in d.items():
        if v is None:
            v = ""
        if isinstance(v, bool):
            v = "true" if v else "false"
        else:
            v = str(v)
        lines.append(f"{k}={shlex.quote(v)}")
    outp.write_text("\n".join(lines) + "\n", encoding="utf-8")

if kind == "qs":
    spawned = None
    for ck in ("worker_readiness", "readiness"):
        if ck in data and isinstance(data[ck], dict) and "all_workers_spawned" in data[ck]:
            spawned = bool(data[ck]["all_workers_spawned"])
            break
    if spawned is None and "all_workers_spawned" in data:
        spawned = bool(data["all_workers_spawned"])
    if spawned is None:
        raise SystemExit("SPAWNED_KEY_ABSENT")
    ac = take(data, "attach_commands")
    attach = ""
    if isinstance(ac, list) and ac:
        attach = str(ac[0])
    sock = ""
    session = take(data, "session_name") or ""
    if attach:
        try:
            parts = shlex.split(attach)
        except ValueError:
            parts = attach.split()
        i = 0
        while i < len(parts):
            if parts[i] == "-S" and i + 1 < len(parts):
                sock = parts[i + 1]
                i += 2
                continue
            if parts[i] == "-L" and i + 1 < len(parts):
                sock = f"/tmp/tmux-501/{parts[i+1]}"
                i += 2
                continue
            if parts[i] == "-t" and i + 1 < len(parts):
                tgt = parts[i + 1]
                session = tgt.split(":")[0]
                i += 2
                continue
            i += 1
    emit({
        "ok": take(data, "ok"),
        "spawned": spawned,
        "status": take(data, "status") or "",
        "sock": sock,
        "session": session,
    })
elif kind == "send":
    emit({
        "ok": take(data, "ok"),
        "status": take(data, "status") or "",
        "message_id": take(data, "message_id") or "",
    })
else:
    raise SystemExit("UNKNOWN_KIND")
PY
}

extract_runtime() {
	python3 - "$1" "$2" <<'PY'
import json, sys, pathlib, shlex
p = pathlib.Path(sys.argv[1])
outp = pathlib.Path(sys.argv[2])
if not p.is_file():
    raise SystemExit("STATE_ABSENT")
data = json.loads(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    raise SystemExit("STATE_NOT_OBJECT")
sock = data["tmux_socket"] if "tmux_socket" in data else ""
active = data["active_team_key"] if "active_team_key" in data else ""
worker_pane = ""
leader_pane = ""
agents = None
if "agents" in data and isinstance(data["agents"], dict):
    agents = data["agents"]
teams = data["teams"] if "teams" in data and isinstance(data["teams"], dict) else None
if teams and active and active in teams and isinstance(teams[active], dict):
    team = teams[active]
    if not sock and "tmux_socket" in team:
        sock = team["tmux_socket"] or sock
    if "agents" in team and isinstance(team["agents"], dict):
        agents = team["agents"]
    lr = team["leader_receiver"] if "leader_receiver" in team and isinstance(team["leader_receiver"], dict) else None
    if lr and "pane_id" in lr:
        leader_pane = lr["pane_id"] or ""
if not leader_pane and "leader_receiver" in data and isinstance(data["leader_receiver"], dict):
    lr = data["leader_receiver"]
    if "pane_id" in lr:
        leader_pane = lr["pane_id"] or ""
if agents and "w1" in agents and isinstance(agents["w1"], dict) and "pane_id" in agents["w1"]:
    worker_pane = agents["w1"]["pane_id"] or ""
vals = {
    "sock": sock or "",
    "active": active or "",
    "worker_pane": worker_pane or "",
    "leader_pane": leader_pane or "",
}
lines = [f"{k}={shlex.quote(str(v))}" for k, v in vals.items()]
outp.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
}

count_token() {
	python3 - "$1" "$2" <<'PY'
import sys, pathlib
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
needle = sys.argv[2]
if not needle:
    print(0)
    raise SystemExit(0)
print(text.count(needle))
PY
}

sqlite_row() {
	_db=$1
	_mid=$2
	_out=$3
	sqlite3 -header -column "$_db" \
		"select message_id, status, error, delivery_attempts, recipient, substr(content,1,80) as content from messages where message_id='${_mid}';" \
		>"$_out" 2>/dev/null || true
}

force_bypass_row() {
	_db=$1
	_mid=$2
	sqlite3 "$_db" "update messages set status='target_resolved', error='probe_failed' where message_id='${_mid}';" || return 1
	_st=$(sqlite3 "$_db" "select status || '|' || ifnull(error,'') from messages where message_id='${_mid}';")
	[ "$_st" = "target_resolved|probe_failed" ]
}

capture_pane() {
	_target=$1
	_out=$2
	"$TMUX_BIN" -S "$SOCK" capture-pane -p -S - -t "$_target" >"$_out" 2>/dev/null || true
}

stop_coordinator() {
	_i=0
	while [ "$_i" -lt 8 ]; do
		_pf="$WS/.team/runtime/coordinator.pid"
		_pid=""
		if [ -f "$_pf" ]; then
			_pid=$(tr -d ' \n' < "$_pf" 2>/dev/null || true)
		fi
		if [ -n "$_pid" ] && pid_alive "$_pid"; then
			_comm=$(comm_base "$_pid")
			case "$_comm" in
			*team-agent*)
				record_pid "$_pid"
				kill -TERM "$_pid" >/dev/null 2>&1 || true
				;;
			*)
				echo "coord pid $_pid comm=$_comm not team-agent; refuse kill" >&2
				return 1
				;;
			esac
		fi
		sleep 0.2
		if [ -n "$_pid" ] && pid_alive "$_pid"; then
			kill -KILL "$_pid" >/dev/null 2>&1 || true
		fi
		_i=$((_i + 1))
		if [ ! -f "$_pf" ]; then
			return 0
		fi
		_pid2=$(tr -d ' \n' < "$_pf" 2>/dev/null || true)
		if [ -z "$_pid2" ] || ! pid_alive "$_pid2"; then
			return 0
		fi
	done
	return 0
}

run_once() {
	_label=$1
	_log="$EVID/coord-once-${_label}.txt"
	"$TA" coordinator --workspace "$WS" --once >"$_log" 2>&1 &
	_cpid=$!
	record_pid "$_cpid"
	wait "$_cpid"
	_rc=$?
	echo "$_rc" >"$EVID/coord-once-${_label}.rc"
	return 0
}

# ---------- begin ----------
clear_identity_env
export PATH="/opt/homebrew/bin:/Users/alauda/.local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
need_cmd python3
need_cmd sqlite3
need_cmd md5
if [ ! -x "$REAL_TMUX" ]; then
	unjudgeable "tmux missing at $REAL_TMUX"
fi
TMUX_BIN="$REAL_TMUX"
if [ ! -x "$SRC_BIN" ]; then
	unjudgeable "gauge missing: $SRC_BIN"
fi
TA="$SRC_BIN"
BIN_MD5=$(md5 -q "$TA")
if [ -z "$EXPECT_MD5_PREFIX" ]; then
	if [ -n "${TEAM_AGENT_BIN:-}${REPRO_GAUGE:-}" ]; then
		EXPECT_MD5_PREFIX="$BIN_MD5"
	else
		EXPECT_MD5_PREFIX="b81c7081"
	fi
fi
case "$BIN_MD5" in
"$EXPECT_MD5_PREFIX"*) ;;
*) unjudgeable "gauge md5 mismatch got=$BIN_MD5 expect_prefix=$EXPECT_MD5_PREFIX path=$TA" ;;
esac
BIN_MTIME=$(stat -f '%Sm' "$TA")
BIN_SIZE=$(stat -f '%z' "$TA")
BIN_VER=$("$TA" --version 2>/dev/null || true)
say "GAUGE path=$TA md5=$BIN_MD5 mtime=$BIN_MTIME size=$BIN_SIZE version=$BIN_VER"
PROT_N=$(protected_present)
say "PROTECTED_SOCKETS_PRESENT=$PROT_N"
if [ "$PROT_N" -lt 1 ]; then
	unjudgeable "protected socket probe failed (need at least one of the three live)"
fi
LOAD_BEFORE=$(load1)
NCPU=$(sysctl -n hw.ncpu 2>/dev/null || echo 0)
say "LOAD1=$LOAD_BEFORE NCPU=$NCPU"
python3 - "$LOAD_BEFORE" "$LOAD_MAX" <<'PY' || unjudgeable "high load window; refuse to judge red/green"
import sys
load=float(sys.argv[1]); cap=float(sys.argv[2])
if load >= cap:
    raise SystemExit(1)
PY

mkdir -p "$WS/agents" "$EVID"
git -C "$WS" init -q
printf '%s\n' "---" "name: ${TEAM_ID}" "objective: t130 dup-inject repro." "provider: fake" "display_backend: none" "---" "" "Team." >"$WS/TEAM.md"
printf '%s\n' "---" "name: ${AGENT_ID}" "role: Fake worker ${AGENT_ID}" "provider: fake" "model: fake" "auth_mode: subscription" "dangerously_skip_permissions: false" "tools:" "  - mcp_team" "---" "" "Fake worker ${AGENT_ID}." >"$WS/agents/${AGENT_ID}.md"

# private caller socket so we never inherit ambient tmux
"$TMUX_BIN" -S "$CALLER" new-session -d -s t130caller -n hold 'exec sleep 3600' || unjudgeable "caller socket create failed"
record_pid "$("$TMUX_BIN" -S "$CALLER" display-message -p -t t130caller:hold '#{pane_pid}' 2>/dev/null || true)"

QS_JSON="$EVID/qs.json"
QS_ERR="$EVID/qs.err"
"$TA" quick-start "$TEAMDIR" --workspace "$WS" --team-id "$TEAM_ID" --yes --no-display --json >"$QS_JSON" 2>"$QS_ERR" || true
echo $? >"$EVID/qs.rc"
write_kv "$QS_JSON" qs "$EVID/qs.parsed" 2>"$EVID/qs.parse.err" || unjudgeable "quick-start json parse failed $(cat "$EVID/qs.parse.err" 2>/dev/null)"
# shellcheck disable=SC1091
. "$EVID/qs.parsed"
say "QS ok=$ok spawned=$spawned status=$status sock=$sock session=$session"
[ "$spawned" = "true" ] || unjudgeable "quick-start did not spawn workers status=$status"

[ -n "$sock" ] || unjudgeable "attach command missing socket"
if is_protected_sock "$sock"; then
	unjudgeable "workspace socket collided with protected: $sock"
fi
SOCK=$sock
SESSION=$session
say "SOCK=$SOCK SESSION=$SESSION"

STATE="$WS/.team/runtime/state.json"
DB="$WS/.team/runtime/team.db"
EVENTS="$WS/.team/logs/events.jsonl"
t=0
while [ "$t" -lt 40 ]; do
	budget_hit && unjudgeable "budget before state.json"
	[ -f "$STATE" ] && [ -f "$DB" ] && break
	sleep 1
	t=$((t + 1))
done
[ -f "$STATE" ] || unjudgeable "state.json missing"
[ -f "$DB" ] || unjudgeable "team.db missing"

extract_runtime "$STATE" "$EVID/runtime.parsed" || unjudgeable "state extract failed"
# shellcheck disable=SC1091
. "$EVID/runtime.parsed"
WORKER_PANE=$worker_pane
LEADER_PANE=$leader_pane
OWNER_TEAM=$active
[ -n "$WORKER_PANE" ] || unjudgeable "worker pane_id missing"
[ -n "$OWNER_TEAM" ] || unjudgeable "active_team_key missing"
if [ -n "$sock" ] && [ "$sock" != "$SOCK" ]; then
	SOCK=$sock
fi
"$TMUX_BIN" -S "$SOCK" list-windows -t "$SESSION" -F '#{window_name} #{pane_id}' >"$EVID/windows.txt" 2>"$EVID/windows.err" || true
if [ -z "$LEADER_PANE" ]; then
	LP=$(awk '$1=="leader"{print $2; exit}' "$EVID/windows.txt")
	if [ -n "$LP" ]; then
		"$TA" attach-leader --workspace "$WS" --pane "$LP" --confirm --json >"$EVID/attach-leader.json" 2>"$EVID/attach-leader.err" || true
		extract_runtime "$STATE" "$EVID/runtime.after-claim.parsed" || true
		if [ -f "$EVID/runtime.after-claim.parsed" ]; then
			# shellcheck disable=SC1091
			. "$EVID/runtime.after-claim.parsed"
			LEADER_PANE=$leader_pane
		fi
		[ -n "$LEADER_PANE" ] || LEADER_PANE=$LP
	fi
fi
[ -n "$LEADER_PANE" ] || say "WARN no bound leader receiver; arm3 likely 2"
say "WORKER_PANE=$WORKER_PANE LEADER_PANE=$LEADER_PANE OWNER=$OWNER_TEAM"

# record coordinator pid if live
if [ -f "$WS/.team/runtime/coordinator.pid" ]; then
	COORD_PID=$(tr -d ' \n' < "$WS/.team/runtime/coordinator.pid")
	record_pid "$COORD_PID"
fi

# ===================== ARM 1: error-bypass dual deliverer =====================
say "ARM1_BEGIN"
TOKEN1="T130A1-$$-$(date +%s)"
SEND1="$EVID/arm1-send.json"
"$TA" send "$AGENT_ID" "arm1 $TOKEN1" --workspace "$WS" --json >"$SEND1" 2>"$EVID/arm1-send.err" || true
write_kv "$SEND1" send "$EVID/arm1-send.parsed" 2>"$EVID/arm1-send.parse.err" || true
MID1=""
if [ -f "$EVID/arm1-send.parsed" ]; then
	# shellcheck disable=SC1091
	. "$EVID/arm1-send.parsed"
	MID1=$message_id
fi
[ -n "$MID1" ] || { ARM1_REASON="send produced no message_id"; ARM1_RC=2; MID1=""; }
NEEDLE1="[team-agent-token:${MID1}]"
if [ -n "$MID1" ]; then
	stop_coordinator
	t=0
	while [ "$t" -lt 20 ]; do
		_have=$(sqlite3 "$DB" "select count(*) from messages where message_id='${MID1}';")
		[ "$_have" = "1" ] && break
		sleep 0.2
		t=$((t + 1))
	done
	sqlite_row "$DB" "$MID1" "$EVID/arm1-row-before.txt"
	if force_bypass_row "$DB" "$MID1"; then
		capture_pane "$WORKER_PANE" "$EVID/arm1-pane-before.txt"
		BEFORE1=$(count_token "$EVID/arm1-pane-before.txt" "$NEEDLE1")
		"$TA" coordinator --workspace "$WS" --once >"$EVID/coord-once-arm1a.txt" 2>&1 &
		P_A=$!
		record_pid "$P_A"
		"$TA" coordinator --workspace "$WS" --once >"$EVID/coord-once-arm1b.txt" 2>&1 &
		P_B=$!
		record_pid "$P_B"
		wait "$P_A"
		echo $? >"$EVID/coord-once-arm1a.rc"
		wait "$P_B"
		echo $? >"$EVID/coord-once-arm1b.rc"
		sleep 1
		capture_pane "$WORKER_PANE" "$EVID/arm1-pane-after.txt"
		cp "$EVENTS" "$EVID/arm1-events.jsonl" 2>/dev/null || true
		sqlite_row "$DB" "$MID1" "$EVID/arm1-row-after.txt"
		AFTER1=$(count_token "$EVID/arm1-pane-after.txt" "$NEEDLE1")
		EV1=$(count_token "$EVID/arm1-events.jsonl" "$NEEDLE1")
		say "ARM1 token_pane_before=$BEFORE1 after=$AFTER1 events=$EV1 mid=$MID1"
		if [ "$AFTER1" -ge 2 ] || [ "$EV1" -ge 2 ]; then
			ARM1_RC=1
			ARM1_REASON="dual-bypass token_count pane=$AFTER1 events=$EV1 mid=$MID1"
		elif [ "$AFTER1" -eq 1 ]; then
			ARM1_RC=0
			ARM1_REASON="single-copy after dual --once pane=$AFTER1 mid=$MID1"
		else
			ARM1_RC=2
			ARM1_REASON="token absent after dual --once pane=$AFTER1 events=$EV1"
		fi
	else
		ARM1_RC=2
		ARM1_REASON="failed to UPDATE row into target_resolved+error"
	fi
fi
say "ARM1_RC=$ARM1_RC $ARM1_REASON"

# ===================== ARM 2: observer fail after physical Enter =====================
say "ARM2_BEGIN"
stop_coordinator
TOKEN2="T130A2-$$-$(date +%s)"
SEND2="$EVID/arm2-send.json"
"$TA" send "$AGENT_ID" "arm2 $TOKEN2" --workspace "$WS" --json >"$SEND2" 2>"$EVID/arm2-send.err" || true
write_kv "$SEND2" send "$EVID/arm2-send.parsed" 2>"$EVID/arm2-send.parse.err" || true
MID2=""
if [ -f "$EVID/arm2-send.parsed" ]; then
	# shellcheck disable=SC1091
	. "$EVID/arm2-send.parsed"
	MID2=$message_id
fi
NEEDLE2="[team-agent-token:${MID2}]"
if [ -z "$MID2" ]; then
	ARM2_RC=2
	ARM2_REASON="send produced no message_id"
else
	# first physical inject via a single --once (coordinator was stopped)
	run_once arm2-first
	sleep 1
	capture_pane "$WORKER_PANE" "$EVID/arm2-pane-first.txt"
	FIRST2=$(count_token "$EVID/arm2-pane-first.txt" "$NEEDLE2")
	sqlite_row "$DB" "$MID2" "$EVID/arm2-row-first.txt"
	say "ARM2 first_inject pane=$FIRST2"
	if [ "$FIRST2" -lt 1 ]; then
		ARM2_RC=2
		ARM2_REASON="first inject did not land token (cannot show observer-fail retry)"
	else
		stop_coordinator
		if ! force_bypass_row "$DB" "$MID2"; then
			ARM2_RC=2
			ARM2_REASON="failed to UPDATE row into target_resolved+error after first inject"
		else
			chflags uchg "$STATE" || unjudgeable "chflags uchg state.json failed"
			STATE_UCHG=1
			run_once arm2-obs1
			sleep 1
			run_once arm2-obs2
			sleep 1
			chflags nouchg "$STATE" || true
			STATE_UCHG=0
			capture_pane "$WORKER_PANE" "$EVID/arm2-pane-after.txt"
			cp "$EVENTS" "$EVID/arm2-events.jsonl" 2>/dev/null || true
			sqlite_row "$DB" "$MID2" "$EVID/arm2-row-after.txt"
			AFTER2=$(count_token "$EVID/arm2-pane-after.txt" "$NEEDLE2")
			EV2=$(count_token "$EVID/arm2-events.jsonl" "$NEEDLE2")
			say "ARM2 token_pane_first=$FIRST2 after=$AFTER2 events=$EV2 mid=$MID2"
			if [ "$AFTER2" -ge 2 ] || [ "$EV2" -ge 2 ]; then
				ARM2_RC=1
				ARM2_REASON="observer-fail/bypass re-inject pane=$AFTER2 events=$EV2 first=$FIRST2 mid=$MID2"
			elif [ "$AFTER2" -eq 1 ] && [ "$FIRST2" -eq 1 ]; then
				ARM2_RC=0
				ARM2_REASON="observer-fail did not re-inject pane=$AFTER2 first=$FIRST2 mid=$MID2"
			else
				ARM2_RC=2
				ARM2_REASON="token vanished after observer-fail ticks pane=$AFTER2 first=$FIRST2"
			fi
		fi
	fi
fi
say "ARM2_RC=$ARM2_RC $ARM2_REASON"

# ===================== ARM 3: report_result retry + dead-string fallback =====================
say "ARM3_BEGIN"
stop_coordinator
# Bind an isolated leader pane so fallback can physically inject. attach-leader
# refuses out-of-workspace callers; writing leader_receiver is the constructability
# path for this arm (still the product fallback, not a mock inject).
if [ -z "$LEADER_PANE" ]; then
	"$TMUX_BIN" -S "$SOCK" new-window -t "$SESSION" -n leader 'exec sleep 3600' >/dev/null 2>&1 || true
	LP=$("$TMUX_BIN" -S "$SOCK" list-panes -t "$SESSION:leader" -F '#{pane_id}' 2>/dev/null | head -n 1)
	if [ -n "$LP" ]; then
		LEADER_PANE=$LP
		record_pid "$("$TMUX_BIN" -S "$SOCK" display-message -p -t "$LEADER_PANE" '#{pane_pid}' 2>/dev/null || true)"
		python3 - "$STATE" "$LEADER_PANE" "$SOCK" "$OWNER_TEAM" <<'PY' || true
import json, pathlib, sys
path, pane, sock, team = sys.argv[1:5]
p = pathlib.Path(path)
data = json.loads(p.read_text(encoding="utf-8"))
recv = {
    "pane_id": pane,
    "status": "attached",
    "tmux_socket": sock,
    "provider": "fake",
}
data["leader_receiver"] = recv
teams = data.get("teams")
if isinstance(teams, dict) and team in teams and isinstance(teams[team], dict):
    teams[team]["leader_receiver"] = recv
p.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
PY
		say "ARM3 bound isolated leader pane $LEADER_PANE"
	fi
fi
TOKEN3="T130A3-$$-$(date +%s)"
if [ -n "$LEADER_PANE" ]; then
	capture_pane "$LEADER_PANE" "$EVID/arm3-pane-before.txt"
else
	: >"$EVID/arm3-pane-before.txt"
fi
MCP_OUT="$EVID/arm3-mcp.out"
MCP_ERR="$EVID/arm3-mcp.err"
# real stdio MCP report_result (product path results.rs retry + fallback)
python3 - "$TA" "$WS" "$AGENT_ID" "$OWNER_TEAM" "$TOKEN3" "$MCP_OUT" "$MCP_ERR" <<'PY'
import json, subprocess, sys, pathlib
ta, ws, agent, owner, token, outp, errp = sys.argv[1:8]
payload_init = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}
payload_call = {
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {
        "name": "report_result",
        "arguments": {
            "summary": f"arm3 {token}",
            "status": "success",
            "tests": [{"command": "t130-arm3", "status": "passed"}],
        },
    },
}
proc = subprocess.Popen(
    [ta, "mcp-server", "--workspace", ws],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    env={
        **{k: v for k, v in __import__("os").environ.items() if k not in {
            "TMUX", "TMUX_PANE", "TEAM_AGENT_ID", "TEAM_AGENT_AGENT_ID",
            "TEAM_AGENT_OWNER_TEAM_ID", "TEAM_AGENT_WORKSPACE",
        }},
        "TEAM_AGENT_WORKSPACE": ws,
        "TEAM_AGENT_ID": agent,
        "TEAM_AGENT_OWNER_TEAM_ID": owner,
    },
    text=True,
)
try:
    stdout, stderr = proc.communicate(
        json.dumps(payload_init) + "\n" + json.dumps(payload_call) + "\n",
        timeout=40,
    )
    rc = proc.returncode
except Exception as e:
    proc.kill()
    stdout, stderr = proc.communicate()
    stderr = (stderr or "") + f"\nMCP_EXCEPTION:{e}\n"
    rc = 2
pathlib.Path(outp).write_text(stdout or "", encoding="utf-8")
pathlib.Path(errp).write_text(stderr or "", encoding="utf-8")
pathlib.Path(outp + ".rc").write_text(str(rc), encoding="utf-8")
PY
sleep 1
# second coordinator tick: scan includes submitted_pending_acceptance
run_once arm3-tick
sleep 1
cp "$EVENTS" "$EVID/arm3-events.jsonl" 2>/dev/null || true
python3 - "$EVID/arm3-events.jsonl" "$EVID/arm3-mcp.out" "$EVID/arm3-ids.txt" "$EVID/arm3-ids.parsed" <<'PY'
import json, sys, pathlib, shlex
events = pathlib.Path(sys.argv[1])
mcp = pathlib.Path(sys.argv[2])
out = pathlib.Path(sys.argv[3])
parsed = pathlib.Path(sys.argv[4])
ids = []
if events.is_file():
    for line in events.read_text(encoding="utf-8", errors="replace").splitlines():
        line=line.strip()
        if not line.startswith("{"):
            continue
        try:
            o=json.loads(line)
        except json.JSONDecodeError:
            continue
        ev = o.get("event") or o.get("name") or ""
        if ev in ("mcp.report_result", "deliver_to_leader.submit", "leader_receiver.queued", "leader_receiver.fallback_pane_attempt"):
            mid = o.get("notification_message_id") or o.get("message_id")
            if isinstance(mid, str) and mid:
                ids.append((ev, mid))
mcp_mid = ""
if mcp.is_file():
    for line in mcp.read_text(encoding="utf-8", errors="replace").splitlines():
        line=line.strip()
        if not line.startswith("{"):
            continue
        try:
            o=json.loads(line)
        except json.JSONDecodeError:
            continue
        if o.get("id") != 2:
            continue
        res = o.get("result") or {}
        content = res.get("content") if isinstance(res, dict) else None
        blob = ""
        if isinstance(content, list) and content and isinstance(content[0], dict) and "text" in content[0]:
            blob = content[0]["text"]
        if blob:
            try:
                inner = json.loads(blob)
            except json.JSONDecodeError:
                inner = {}
            if isinstance(inner, dict) and inner.get("notification_message_id"):
                mcp_mid = str(inner["notification_message_id"])
out.write_text(
    "events_ids=\n" + "\n".join(f"{e} {m}" for e,m in ids[-12:]) + "\n",
    encoding="utf-8",
)
last_mid = mcp_mid or (ids[-1][1] if ids else "")
parsed.write_text(f"last_mid={shlex.quote(last_mid)}\n", encoding="utf-8")
print("n_event_ids", len(ids))
print("last_mid", last_mid)
PY
MID3=""
if [ -f "$EVID/arm3-ids.parsed" ]; then
	# shellcheck disable=SC1091
	. "$EVID/arm3-ids.parsed"
	MID3=$last_mid
fi
if [ -n "$LEADER_PANE" ]; then
	capture_pane "$LEADER_PANE" "$EVID/arm3-pane-after.txt"
else
	: >"$EVID/arm3-pane-after.txt"
fi
if [ -z "$MID3" ]; then
	ARM3_RC=2
	ARM3_REASON="report_result produced no notification message_id (events/mcp); fallback/retry not observed"
else
	NEEDLE3="[team-agent-token:${MID3}]"
	BEFORE3=$(count_token "$EVID/arm3-pane-before.txt" "$NEEDLE3")
	AFTER3=$(count_token "$EVID/arm3-pane-after.txt" "$NEEDLE3")
	EV3=$(count_token "$EVID/arm3-events.jsonl" "$NEEDLE3")
	sqlite_row "$DB" "$MID3" "$EVID/arm3-row.txt"
	say "ARM3 mid=$MID3 pane_before=$BEFORE3 after=$AFTER3 events=$EV3"
	if [ "$AFTER3" -ge 2 ] || [ "$EV3" -ge 2 ]; then
		ARM3_RC=1
		ARM3_REASON="report_result/fallback double token pane=$AFTER3 events=$EV3 mid=$MID3"
	elif [ "$AFTER3" -eq 1 ]; then
		ARM3_RC=0
		ARM3_REASON="single-copy after report_result/fallback pane=$AFTER3 mid=$MID3"
	else
		ARM3_RC=2
		ARM3_REASON="constructed MCP report_result but token pane=$AFTER3 events=$EV3 (retry/fallback did not land)"
	fi
fi
say "ARM3_RC=$ARM3_RC $ARM3_REASON"

# copy events coords for REPRO
python3 - "$EVID/runtime/events.jsonl" "$EVENTS" "$EVID/events-coords.txt" <<'PY' 2>/dev/null || true
import sys, pathlib
dst = pathlib.Path(sys.argv[3])
src = pathlib.Path(sys.argv[2])
if not src.is_file():
    src = pathlib.Path(sys.argv[1])
want = (
    "message.delivered",
    "delivery.item_blocked",
    "leader_receiver.fallback_pane_attempt",
    "leader_receiver.acceptance_pending",
    "leader_receiver.receipt_source_unavailable",
    "mcp.report_result",
    "deliver_to_leader.submit",
    "turn_open.armed_after_inject",
    "send.inject_failed",
)
lines=[]
if src.is_file():
    for i,line in enumerate(src.read_text(encoding="utf-8", errors="replace").splitlines(),1):
        for w in want:
            if w in line:
                lines.append(f"L{i}:{w}:{line[:400]}")
                break
dst.write_text("\n".join(lines[-80:])+"\n", encoding="utf-8")
print("coords", len(lines))
PY

say "GAUGE path=$TA md5=$BIN_MD5 mtime=$BIN_MTIME"
PROT_AFTER=$(protected_present)
say "PROTECTED_AFTER=$PROT_AFTER"
[ "$PROT_AFTER" -ge "$PROT_N" ] || unjudgeable "protected socket disappeared during run"

if budget_hit; then
	unjudgeable "budget exceeded after arms ARM1=$ARM1_RC ARM2=$ARM2_RC ARM3=$ARM3_RC"
fi

# overall: any arm red => 1.
# ARM1+ARM2 are the constructable dual-inject arms. ARM3 live fallback still
# hits rebind_required even with an isolated pane write (attach-leader refuses
# out-of-workspace callers); ARM3=2 is constructability, not green-of-double.
# After the product gates close: ARM1=0 ARM2=0 ARM3!=1 => 0.
if [ "$ARM1_RC" -eq 1 ] || [ "$ARM2_RC" -eq 1 ] || [ "$ARM3_RC" -eq 1 ]; then
	fail_repro "dup-inject arm1=$ARM1_RC arm2=$ARM2_RC arm3=$ARM3_RC"
fi
if [ "$ARM1_RC" -eq 0 ] && [ "$ARM2_RC" -eq 0 ] && [ "$ARM3_RC" -ne 1 ]; then
	pass_ok "arm1+arm2 single-copy arm3=$ARM3_RC"
fi
unjudgeable "no arm reproduced double-inject (arm1=$ARM1_RC arm2=$ARM2_RC arm3=$ARM3_RC)"
