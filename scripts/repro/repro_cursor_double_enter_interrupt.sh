#!/bin/sh
# //!
# //! purpose: 固化 cursor 注入双回车打断（先红）——真实 HOME + 隔离
# //!          workspace 起 1 个 cursor_agent subscription 席。先投长任务
# //!          等到回合进行中（屏上 Working），再经 team-agent send 注入
# //!          第二条。重试臂若再按 Enter，cursor 会把进行中回合打断。
# //!          打断 = exit 1；长任务完整结束且第二条在边界被消费 = 0；
# //!          起不了席 / 登录墙 / 未进入 busy / 触发不了重试 = 2。
# //! contract:
# //!   provides:
# //!     - name: cursor-double-enter-interrupt-repro
# //!       what: 一条命令，退出码即判据；修前须红、修后须绿；同一装置验收
# //!   requires:
# //!     - name: team-agent-0.5.66
# //!       what: 默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!             md5 b81c70816ff504d44f1d4a041373c84f；TEAM_AGENT_BIN 可覆盖
# //!     - name: cursor-subscription
# //!       what: 本机真实 HOME 已登录；不读 .env / chats 正文 / 不打印 proxy 值
# //! boundary:
# //!   - 三态：0 需求已满足 / 1 复现(打断) / 2 不可判；禁止把 2 折进 1
# //!   - 真实 HOME，禁止 export HOME=
# //!   - qs 到达看 all_workers_spawned，不看 rc=0
# //!   - 隔离临时 workspace + 私有 caller socket；不继承 raw TMUX/TMUX_PANE
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的精确 PID
# //!   - 不造假红：重试臂未执行不得报 1
# //!   - 预算：单次 300s；超时 exit 2。高载窗下的红记 2
# //!   - POSIX：禁 bash 进程替换；proxy 只报 present/len；订阅 ≤10 请求
# //! maturity: wired
#
# 用法: sh repro_cursor_double_enter_interrupt.sh
# 环境: TEAM_AGENT_BIN  KEEP_TMP=1  PROMPT_TIMEOUT  BUSY_TIMEOUT  AFTER_TIMEOUT

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="b81c70816ff504d44f1d4a041373c84f"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-t126-repro-$$"
TEAM_ID="t126r$$"
AGENT_ID="w1"
LONG_TOKEN="T126L-$$"
SHORT_TOKEN="T126S-$$"
CALLER="$RUN/caller.sock"
WS="$RUN/ws"
TEAMDIR="$WS/t126team"
STAMP=$(date +%Y%m%dT%H%M%S)
EVID="$NODE/runs/$STAMP"
PROMPT_TIMEOUT="${PROMPT_TIMEOUT:-90}"
BUSY_TIMEOUT="${BUSY_TIMEOUT:-90}"
AFTER_TIMEOUT="${AFTER_TIMEOUT:-90}"
BUDGET_SEC="${BUDGET_SEC:-300}"
PROTECTED="ta-a9fd5b7defbd ta-a0afa5f9c7f6 ta-b7cc1c640ccf"
PIDS=""
SOCK=""
SESSION=""
TARGET=""
STARTED=$(date +%s)
LOAD_BEFORE=""
NCPU=""
CLEANED=0
TA=""
BIN_MD5=""
TMUX_BIN=""
SEND2_START=0

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
	NOW=$(date +%s)
	[ $((NOW - STARTED)) -ge "$BUDGET_SEC" ]
}

copy_out() {
	[ -n "${1:-}" ] && [ -e "$1" ] || return 0
	mkdir -p "$EVID"
	cp "$1" "$EVID/$(basename "$1")" 2>/dev/null || true
}

proxy_gauge() {
	python3 - <<'PY'
import os
names = (
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
    "http_proxy", "https_proxy", "all_proxy",
)
for n in names:
    if n in os.environ:
        v = os.environ[n]
        print(f"proxy_{n}=present len={len(v)}")
    else:
        print(f"proxy_{n}=absent")
PY
}

finish() {
	_rc=$1
	NOW=$(date +%s)
	ELAP=$((NOW - STARTED))
	LOAD_AFTER=$(load1)
	PROT_AFTER=$(protected_present)
	echo "elapsed_sec=$ELAP"
	echo "load_after=$LOAD_AFTER"
	echo "protected_after=$PROT_AFTER"
	echo "bin_path=$TA"
	echo "bin_md5=${BIN_MD5:-unknown}"
	echo "verdict_rc=$_rc"
	if [ "$_rc" -eq 1 ] && [ -n "$NCPU" ]; then
		_over=$(awk -v l="${LOAD_BEFORE:-0}" -v n="$NCPU" 'BEGIN {print (l+0 > n+0) ? "yes" : "no"}')
		if [ "$_over" = "yes" ]; then
			echo "UNJUDGEABLE: high-load window (load1=$LOAD_BEFORE ncpu=$NCPU); red would be false-red" >&2
			_rc=2
			echo "verdict_rc=$_rc"
		fi
	fi
	mkdir -p "$EVID"
	{
		echo "verdict_rc=$_rc"
		echo "elapsed_sec=$ELAP"
		echo "long_token=$LONG_TOKEN"
		echo "short_token=$SHORT_TOKEN"
		echo "team_id=$TEAM_ID"
		echo "ws=$WS"
		echo "bin_path=$TA"
		echo "bin_md5=${BIN_MD5:-unknown}"
	} >"$EVID/SUMMARY.txt"
	cleanup
	exit "$_rc"
}

cleanup() {
	[ "$CLEANED" -eq 1 ] && return 0
	CLEANED=1
	if [ -n "${TA:-}" ] && [ -x "${TA:-}" ] && [ -d "$WS" ]; then
		"$TA" shutdown --workspace "$WS" --team "$TEAM_ID" --keep-logs --json >"$RUN/shutdown.json" 2>"$RUN/shutdown.err" || true
		copy_out "$RUN/shutdown.json"
	fi
	if [ -n "${SOCK:-}" ] && [ -S "${SOCK:-}" ] && ! is_protected_sock "$SOCK"; then
		"$REAL_TMUX" -S "$SOCK" kill-server >/dev/null 2>&1 || true
	fi
	if [ -S "$CALLER" ]; then
		"$REAL_TMUX" -S "$CALLER" kill-server >/dev/null 2>&1 || true
	fi
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(comm_base "$_p")
			case "$_comm" in
			tmux|team-agent|bash|sh)
				kill -TERM "$_p" >/dev/null 2>&1 || true
				;;
			*)
				echo "cleanup: skip pid $_p comm=${_comm:-unknown} (not in allow-list)" >&2
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
	if [ "${KEEP_TMP:-0}" != "1" ] && [ -d "$RUN" ]; then
		if [ -d "$WS/.team" ]; then
			mkdir -p "$EVID/runtime"
			[ -f "$WS/.team/runtime/state.json" ] && cp "$WS/.team/runtime/state.json" "$EVID/runtime/state.json" 2>/dev/null || true
			find "$WS/.team" -name 'events.jsonl' -exec cp {} "$EVID/runtime/events.jsonl" \; 2>/dev/null || true
		fi
		rm -rf "$RUN"
	fi
}

trap 'cleanup' EXIT INT TERM

need_cmd() {
	command -v "$1" >/dev/null 2>&1 || unjudgeable "$1 not on PATH"
}

json_field() {
	_file=$1
	_kind=$2
	python3 - "$_file" "$_kind" <<'PY'
import json, sys, pathlib
path = pathlib.Path(sys.argv[1])
kind = sys.argv[2]
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
    ok = take(data, "ok")
    reason = take(data, "reason") or ""
    status = take(data, "status") or ""
    attach = ""
    ac = take(data, "attach_commands")
    if isinstance(ac, list) and ac:
        attach = str(ac[0])
    session = take(data, "session_name") or ""
    print(f"ok={ok}")
    print(f"spawned={str(spawned).lower()}")
    print(f"reason={reason}")
    print(f"status={status}")
    print(f"attach={attach}")
    print(f"session={session}")
elif kind == "send":
    ok = take(data, "ok")
    status = take(data, "status") or ""
    delivered = take(data, "delivered")
    print(f"ok={ok}")
    print(f"status={status}")
    print(f"delivered={delivered}")
else:
    raise SystemExit("UNKNOWN_KIND")
PY
}

parse_attach() {
	python3 - <<'PY'
import sys, shlex
line = sys.stdin.read().strip()
if not line:
    raise SystemExit(0)
try:
    parts = shlex.split(line)
except ValueError:
    parts = line.split()
sock = ""
session = ""
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
        session = parts[i + 1]
        i += 2
        continue
    i += 1
print(f"sock={sock}")
print(f"session={session}")
PY
}

filter_inject_events() {
	python3 - "$1" "$2" <<'PY'
import json, sys, pathlib
src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
want = {
    "send.unverified",
    "send.failed",
    "send.inject_failed",
    "send.deferred_busy",
    "message.delivered",
    "turn_open.armed_after_inject",
    "turn_open.armed_after_delivery",
}
out = []
max_attempts = 0
max_idx = 0
if src.is_file():
    for ln in src.read_text(encoding="utf-8", errors="replace").splitlines():
        ln = ln.strip()
        if not ln.startswith("{"):
            continue
        try:
            ev = json.loads(ln)
        except json.JSONDecodeError:
            continue
        if not isinstance(ev, dict) or "event" not in ev:
            continue
        name = ev["event"]
        if name not in want:
            continue
        row = {"event": name}
        for k in (
            "message_id",
            "recipient",
            "reason",
            "attempts",
            "max_attempts",
            "status",
            "ts",
            "verification",
        ):
            if k in ev:
                row[k] = ev[k]
        if "attempts" in ev:
            try:
                a = int(ev["attempts"])
                if a > max_attempts:
                    max_attempts = a
            except (TypeError, ValueError):
                pass
        detail = ev.get("submit_attempts_detail")
        if isinstance(detail, list):
            row["detail_n"] = len(detail)
            idxs = []
            for item in detail:
                if isinstance(item, dict) and "attempt_index" in item:
                    try:
                        idxs.append(int(item["attempt_index"]))
                    except (TypeError, ValueError):
                        pass
            if idxs:
                row["attempt_indexes"] = idxs
                mx = max(idxs)
                if mx > max_idx:
                    max_idx = mx
        out.append(row)
dst.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"inject_events_n={len(out)}")
print(f"events_max_attempts={max_attempts}")
print(f"events_max_attempt_index={max_idx}")
for row in out:
    extra = ""
    if "attempts" in row:
        extra += f" attempts={row['attempts']}"
    if "reason" in row:
        extra += f" reason={row['reason']}"
    print(f"ev={row['event']}{extra}")
PY
}

count_enters_since() {
	python3 - "$1" "$2" <<'PY'
import sys, pathlib
log = pathlib.Path(sys.argv[1])
start = int(sys.argv[2])
enters = 0
escapes = 0
if log.is_file():
    for ln in log.read_text(encoding="utf-8", errors="replace").splitlines():
        ts = None
        if ln.startswith("ts="):
            rest = ln[3:]
            num = rest.split(" ", 1)[0]
            try:
                ts = int(num)
            except ValueError:
                ts = None
        if ts is None or ts < start:
            continue
        # keys field only; wrapper never dumps paste/load-buffer.
        # tmux may emit Enter as the name "Enter" or as C-m.
        n_enter = ln.count("Enter")
        n_cm = ln.count("C-m")
        enters += n_enter + n_cm
        if "Escape" in ln:
            escapes += ln.count("Escape")
print(f"enter_count={enters}")
print(f"escape_count={escapes}")
PY
}

wait_file() {
	_path=$1
	_n=$2
	_i=0
	while [ "$_i" -lt "$_n" ]; do
		budget_hit && return 1
		[ -f "$_path" ] && return 0
		sleep 1
		_i=$((_i + 1))
	done
	return 1
}

pane_text() {
	"$REAL_TMUX" -S "$SOCK" capture-pane -p -t "$TARGET" -S - -E - 2>/dev/null || true
}

save_pane() {
	_dest=$1
	pane_text >"$_dest" 2>/dev/null || true
	copy_out "$_dest"
}

has_login_wall() {
	_txt=$1
	echo "$_txt" | grep -Eiq 'Press any key to log in|please log in|not authenticated|sign in' && return 0
	return 1
}

has_cursor_chrome() {
	_txt=$1
	echo "$_txt" | grep -Fq 'Cursor Agent' && return 0
	echo "$_txt" | grep -Fq 'Plan, search, build' && return 0
	return 1
}

pane_busy() {
	_txt=$1
	echo "$_txt" | grep -Fq 'Working' && return 0
	echo "$_txt" | grep -Fq 'Thinking' && return 0
	echo "$_txt" | grep -Eq 'esc to interrupt|Esc to interrupt|ctrl\+c to stop' && return 0
	return 1
}

pane_stopped() {
	_txt=$1
	echo "$_txt" | grep -Eiq 'stopped|interrupted|enter send now' && return 0
	return 1
}

wait_prompt() {
	_secs=$1
	_label=$2
	_i=0
	while [ "$_i" -lt "$_secs" ]; do
		budget_hit && return 1
		_txt=$(pane_text)
		if has_login_wall "$_txt"; then
			save_pane "$RUN/pane-${_label}-loginwall.txt"
			unjudgeable "login/trust wall on pane during $_label"
		fi
		if has_cursor_chrome "$_txt"; then
			save_pane "$RUN/pane-${_label}-prompt.txt"
			return 0
		fi
		sleep 2
		_i=$((_i + 2))
	done
	save_pane "$RUN/pane-${_label}-timeout.txt"
	return 1
}

wait_busy_long() {
	_secs=$1
	_i=0
	while [ "$_i" -lt "$_secs" ]; do
		budget_hit && return 1
		_txt=$(pane_text)
		if has_login_wall "$_txt"; then
			save_pane "$RUN/pane-busy-loginwall.txt"
			unjudgeable "login wall while waiting busy"
		fi
		if echo "$_txt" | grep -F -q "$LONG_TOKEN"; then
			if pane_busy "$_txt"; then
				save_pane "$RUN/pane-busy.txt"
				return 0
			fi
		fi
		sleep 1
		_i=$((_i + 1))
	done
	save_pane "$RUN/pane-busy-timeout.txt"
	return 1
}

find_target() {
	_sess=$1
	_win=$("$REAL_TMUX" -S "$SOCK" list-windows -t "$_sess" -F '#{window_name}' 2>/dev/null || true)
	echo "windows=$_win" >&2
	echo "$_win" | grep -qx "$AGENT_ID" && echo "$_sess:$AGENT_ID" && return 0
	_first=$(echo "$_win" | awk 'NF{print; exit}')
	if [ -n "$_first" ]; then
		echo "$_sess:$_first"
		return 0
	fi
	return 1
}

install_tmux_wrapper() {
	mkdir -p "$RUN/bin"
	cat >"$RUN/bin/tmux" <<EOF
#!/bin/sh
LOG="\${TA_TMUX_KEYLOG:-/dev/null}"
is_send=0
keys=""
for a in "\$@"; do
	case "\$a" in
	send-keys) is_send=1 ;;
	Enter|Escape|C-c|C-m|C-u|C-M) keys="\$keys \$a" ;;
	esac
done
if [ "\$is_send" -eq 1 ] && [ -n "\$keys" ]; then
	echo "ts=\$(date +%s) keys=\$keys" >> "\$LOG"
fi
exec "$REAL_TMUX" "\$@"
EOF
	chmod +x "$RUN/bin/tmux"
	TMUX_BIN="$RUN/bin/tmux"
	export TA_TMUX_KEYLOG="$RUN/tmux-keys.log"
	: >"$RUN/tmux-keys.log"
}

# ── preflight ──────────────────────────────────────────────
need_cmd python3
need_cmd md5
[ -x "$SRC_BIN" ] || unjudgeable "team-agent not executable: $SRC_BIN"
[ -x "$REAL_TMUX" ] || unjudgeable "tmux not executable: $REAL_TMUX"
[ -x /Users/alauda/.local/bin/agent ] || unjudgeable "PATH agent missing: /Users/alauda/.local/bin/agent"

unset TMUX
unset TMUX_PANE

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$EVID" /tmp/ta-t126-bin
cp "$SRC_BIN" /tmp/ta-t126-bin/team-agent
chmod +x /tmp/ta-t126-bin/team-agent
TA=/tmp/ta-t126-bin/team-agent
BIN_MD5=$(md5 -q "$TA")
BIN_MTIME=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S' "$TA")
BIN_SIZE=$(stat -f '%z' "$TA")
BIN_VER=$("$TA" --version 2>/dev/null | head -n 1)
if [ -z "${TEAM_AGENT_BIN:-}" ] && [ "$BIN_MD5" != "$EXPECT_MD5" ]; then
	unjudgeable "default runtime md5=$BIN_MD5 expected=$EXPECT_MD5"
fi

install_tmux_wrapper
SAFE_PATH="$RUN/bin:/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin"
export PATH="$SAFE_PATH"

NCPU=$(/usr/sbin/sysctl -n hw.ncpu 2>/dev/null || echo 0)
LOAD_BEFORE=$(load1)
PROT_BEFORE=$(protected_present)
[ "$PROT_BEFORE" -eq 3 ] || unjudgeable "protected sockets present=$PROT_BEFORE (need 3); refusing to run"

{
	echo "probe=cursor-double-enter-interrupt"
	echo "src_bin=$SRC_BIN"
	echo "bin_path=$TA"
	echo "bin_md5=$BIN_MD5"
	echo "bin_mtime=$BIN_MTIME"
	echo "bin_size=$BIN_SIZE"
	echo "bin_version=$BIN_VER"
	echo "expect_md5=$EXPECT_MD5"
	echo "load_before=$LOAD_BEFORE"
	echo "ncpu=$NCPU"
	echo "protected_before=$PROT_BEFORE"
	echo "long_token=$LONG_TOKEN"
	echo "short_token=$SHORT_TOKEN"
	echo "team_id=$TEAM_ID"
	echo "ws=$WS"
	echo "home_isolated=false"
	echo "budget_sec=$BUDGET_SEC"
	proxy_gauge
} | tee "$EVID/gauge.txt"

echo "load_before=$LOAD_BEFORE ncpu=$NCPU"

cat >"$TEAMDIR/TEAM.md" <<EOF
---
name: ${TEAM_ID}
objective: isolated cursor double-enter interrupt repro (one worker)
dangerous_auto_approve: false
fast: false
---

Team config only.
EOF

cat >"$TEAMDIR/agents/${AGENT_ID}.md" <<EOF
---
name: ${AGENT_ID}
role: Cursor Repro Worker
provider: cursor_agent
auth_mode: subscription
dangerously_skip_permissions: true
model: sonnet-4-thinking
tools:
  - mcp_team
  - provider_builtin
---

You are a disposable repro worker. Do not search the workspace. Do not call any tools. Do not call get_team_status. If you see a counting task, print the numbers in the pane. If you see a short token, reply with that token only.
EOF

"$REAL_TMUX" -S "$CALLER" new-session -d -s t126caller -c "$WS" -- /bin/bash
CALLER_SERVER_PID=$("$REAL_TMUX" -S "$CALLER" display-message -p -t t126caller '#{pid}' 2>/dev/null || true)
CALLER_PANE_PID=$("$REAL_TMUX" -S "$CALLER" display-message -p -t t126caller '#{pane_pid}' 2>/dev/null || true)
record_pid "$CALLER_SERVER_PID"
record_pid "$CALLER_PANE_PID"
echo "caller_sock=$CALLER"
echo "caller_server_pid=$CALLER_SERVER_PID"
echo "caller_pane_pid=$CALLER_PANE_PID"
narrow_ps "$CALLER_SERVER_PID" | tee "$EVID/caller-ps.txt"

QS_RC_FILE="$RUN/qs.rc"
"$REAL_TMUX" -S "$CALLER" new-window -t t126caller -n launch -c "$WS" -- /bin/sh -c "
PATH='$SAFE_PATH'
export PATH
unset TMUX_PANE
export TA_TMUX_KEYLOG='$RUN/tmux-keys.log'
'$TA' quick-start '$TEAMDIR' --workspace '$WS' --team-id '$TEAM_ID' --name '$TEAM_ID' --yes --no-display --json >'$RUN/qs.json' 2>'$RUN/qs.err'
echo \$? >'$QS_RC_FILE'
"
if ! wait_file "$QS_RC_FILE" 90; then
	copy_out "$RUN/qs.json"
	copy_out "$RUN/qs.err"
	unjudgeable "quick-start did not finish within 90s"
fi
QS_RC=$(cat "$QS_RC_FILE")
copy_out "$RUN/qs.json"
copy_out "$RUN/qs.err"
echo "quickstart_rc=$QS_RC"
echo "quickstart_rc=$QS_RC" >"$EVID/quickstart.rc"

QS_INFO=$(json_field "$RUN/qs.json" qs 2>"$RUN/qs.parse.err" || echo "PARSE_FAIL")
echo "$QS_INFO" | tee "$EVID/qs.parsed"
SPAWNED=$(echo "$QS_INFO" | awk -F= '/^spawned=/{print $2; exit}')
ATTACH=$(echo "$QS_INFO" | awk -F= '/^attach=/{sub(/^attach=/,""); print; exit}')
SESSION=$(echo "$QS_INFO" | awk -F= '/^session=/{print $2; exit}')
REASON=$(echo "$QS_INFO" | awk -F= '/^reason=/{sub(/^reason=/,""); print; exit}')

if [ "$QS_INFO" = "PARSE_FAIL" ] || [ "$QS_INFO" = "FILE_ABSENT" ] || [ "$QS_INFO" = "SPAWNED_KEY_ABSENT" ]; then
	unjudgeable "quick-start json unreadable rc=$QS_RC info=$QS_INFO"
fi

if [ "$SPAWNED" != "true" ]; then
	unjudgeable "workers not spawned spawned=$SPAWNED rc=$QS_RC reason=$REASON"
fi
echo "note=quick-start may be rc=1 for leader_receiver_unbound; workers spawned so continue"

PARSED=$(printf '%s\n' "$ATTACH" | parse_attach)
SOCK=$(echo "$PARSED" | awk -F= '/^sock=/{print $2; exit}')
if [ -z "$SESSION" ]; then
	SESSION=$(echo "$PARSED" | awk -F= '/^session=/{print $2; exit}')
fi
[ -n "$SOCK" ] || SOCK=$CALLER
[ -n "$SESSION" ] || SESSION="team-$TEAM_ID"
if is_protected_sock "$SOCK"; then
	unjudgeable "attach socket is protected: $SOCK — refusing to use"
fi
echo "tmux_sock=$SOCK"
echo "tmux_session=$SESSION"
echo "tmux_sock=$SOCK" >>"$EVID/gauge.txt"
echo "tmux_session=$SESSION" >>"$EVID/gauge.txt"

_i=0
while [ "$_i" -lt 30 ]; do
	budget_hit && unjudgeable "budget before session appeared"
	"$REAL_TMUX" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$REAL_TMUX" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "tmux session $SESSION missing on $SOCK"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window on $SESSION"
echo "target=$TARGET"
echo "target=$TARGET" >>"$EVID/gauge.txt"

if ! wait_prompt "$PROMPT_TIMEOUT" before_send; then
	unjudgeable "cursor chrome not seen within ${PROMPT_TIMEOUT}s before send"
fi

# 完成暗号不得出现在用户气泡：模型拼 END- + 首行 token，提示词里没有整串。
END_MARK="END-${LONG_TOKEN}"
SHORT_ACK="${SHORT_TOKEN}-OK"
echo "end_mark=$END_MARK" >>"$EVID/gauge.txt"
echo "short_ack=$SHORT_ACK" >>"$EVID/gauge.txt"

# ① 长任务，制造进行中回合。指令里不出现 ACK 等待串。
SEND1_START=$(date +%s)
SEND1_MSG="${LONG_TOKEN}
Do not call tools. Print the first-line token. Then print N-1 N-2 ... N-80 each on its own line. After N-80 print END- followed immediately by the first-line token. Nothing else."
PATH="$SAFE_PATH" TA_TMUX_KEYLOG="$RUN/tmux-keys.log" \
"$TA" send "$AGENT_ID" "$SEND1_MSG" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send1.json" 2>"$RUN/send1.err"
SEND1_RC=$?
copy_out "$RUN/send1.json"
copy_out "$RUN/send1.err"
echo "send1_rc=$SEND1_RC"
SEND1_INFO=$(json_field "$RUN/send1.json" send 2>/dev/null || echo "ok=missing")
echo "$SEND1_INFO" | tee "$EVID/send1.parsed"
SEND1_OK=$(echo "$SEND1_INFO" | awk -F= '/^ok=/{print $2; exit}')
if [ "$SEND1_OK" != "True" ] && [ "$SEND1_OK" != "true" ]; then
	unjudgeable "send1 failed rc=$SEND1_RC ok=$SEND1_OK"
fi

if ! wait_busy_long "$BUSY_TIMEOUT"; then
	unjudgeable "in-progress turn (Working + long token) not observed within ${BUSY_TIMEOUT}s — retry path not reachable"
fi
echo "busy_reached=yes"

save_pane "$RUN/pane-before-send2.txt"
COUNT_BEFORE=$(grep -c 'N-12' "$RUN/pane-before-send2.txt" || true)
echo "n12_before=$COUNT_BEFORE" | tee "$EVID/count-before.txt"

# sidecar: 打断瞬间屏录
SIDECAR_RC="$RUN/sidecar.rc"
"$REAL_TMUX" -S "$CALLER" new-window -t t126caller -n ticks -c "$RUN" -- /bin/sh -c "
i=0
while [ \$i -lt 50 ]; do
	'$REAL_TMUX' -S '$SOCK' capture-pane -p -t '$TARGET' -S - -E - >'$RUN/pane-tick-'\$i'.txt' 2>/dev/null || true
	i=\$((i + 1))
	sleep 1
done
echo 0 >'$SIDECAR_RC'
"

SEND1_ENTER=$(count_enters_since "$RUN/tmux-keys.log" "$SEND1_START" 2>/dev/null || echo "enter_count=0")
echo "$SEND1_ENTER" | tee "$EVID/enter-send1.txt"

SEND2_START=$(date +%s)
# 大 payload：token 落在末尾，尽量让消费轮询在 busy 时仍看见 marker（任务书允许）。
SEND2_PAD=$(python3 - "$SHORT_TOKEN" <<'PY'
import sys
tok = sys.argv[1]
lines = [f"PAD-{i:03d}-{tok}" for i in range(40)]
sys.stdout.write("\n".join(lines))
PY
)
SEND2_MSG="${SHORT_TOKEN}
Do not call tools. Reply with the token plus the suffix -OK.
${SEND2_PAD}"
PATH="$SAFE_PATH" TA_TMUX_KEYLOG="$RUN/tmux-keys.log" \
"$TA" send "$AGENT_ID" "$SEND2_MSG" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send2.json" 2>"$RUN/send2.err"
SEND2_RC=$?
SEND2_QUEUE_END=$(date +%s)
copy_out "$RUN/send2.json"
copy_out "$RUN/send2.err"
echo "send2_rc=$SEND2_RC"
echo "send2_queue_elapsed_sec=$((SEND2_QUEUE_END - SEND2_START))" | tee "$EVID/send2-elapsed.txt"
SEND2_INFO=$(json_field "$RUN/send2.json" send 2>/dev/null || echo "ok=missing")
echo "$SEND2_INFO" | tee "$EVID/send2.parsed"
SEND2_OK=$(echo "$SEND2_INFO" | awk -F= '/^ok=/{print $2; exit}')
if [ "$SEND2_OK" != "True" ] && [ "$SEND2_OK" != "true" ]; then
	unjudgeable "send2 persist failed rc=$SEND2_RC ok=$SEND2_OK"
fi

# send 返回 queued 不是注入。等 SHORT_TOKEN 上屏 = 注入到达，再留 5s 给重试臂。
INJECT2=0
_i=0
while [ "$_i" -lt 45 ]; do
	budget_hit && break
	_txt=$(pane_text)
	if echo "$_txt" | grep -F -q "$SHORT_TOKEN"; then
		INJECT2=1
		save_pane "$RUN/pane-send2-visible.txt"
		if pane_busy "$_txt"; then
			echo "send2_while_busy=yes" | tee "$EVID/send2-busy.txt"
		else
			echo "send2_while_busy=no" | tee "$EVID/send2-busy.txt"
		fi
		break
	fi
	sleep 1
	_i=$((_i + 1))
done
echo "send2_token_visible=$INJECT2"
# retry poll is 12*100ms per attempt, up to 3 → leave 5s after first visibility
sleep 5
save_pane "$RUN/pane-after-send2.txt"
copy_out "$RUN/tmux-keys.log"

ENTER_INFO=$(count_enters_since "$RUN/tmux-keys.log" "$SEND2_START" 2>/dev/null || echo "enter_count=0")
echo "$ENTER_INFO" | tee "$EVID/enter-send2.txt"
ENTER_N=$(echo "$ENTER_INFO" | awk -F= '/^enter_count=/{print $2; exit}')
[ -n "$ENTER_N" ] || ENTER_N=0

EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events.jsonl"
	filter_inject_events "$RUN/events.jsonl" "$EVID/events-inject.json" | tee "$EVID/events-inject.summary"
	copy_out "$RUN/events.jsonl"
else
	echo "events.jsonl=ABSENT" | tee "$EVID/events-inject.summary"
	echo "[]" >"$EVID/events-inject.json"
fi
EV_ATT=$(awk -F= '/^events_max_attempts=/{print $2; exit}' "$EVID/events-inject.summary" 2>/dev/null || echo 0)
EV_IDX=$(awk -F= '/^events_max_attempt_index=/{print $2; exit}' "$EVID/events-inject.summary" 2>/dev/null || echo 0)
[ -n "$EV_ATT" ] || EV_ATT=0
[ -n "$EV_IDX" ] || EV_IDX=0

DEFERRED=$(grep -c 'ev=send.deferred_busy' "$EVID/events-inject.summary" 2>/dev/null || true)

if [ "$INJECT2" -ne 1 ]; then
	if [ "$DEFERRED" -gt 0 ] 2>/dev/null; then
		unjudgeable "send2 deferred_busy; inject retry path not executed enter_count=$ENTER_N"
	fi
	unjudgeable "send2 token never appeared on pane (inject not observed) enter_count=$ENTER_N"
fi

# ③ 等结局：打断 vs 边界消费。完成暗号是模型拼出来的 END-<token> / <token>-OK。
_i=0
DONE_LONG=0
SHORT_ACKED=0
SHORT_SEEN=1
STOPPED=0
BUSY_AFTER=0
while [ "$_i" -lt "$AFTER_TIMEOUT" ]; do
	budget_hit && break
	_txt=$(pane_text)
	echo "$_txt" | grep -F -q "$END_MARK" && DONE_LONG=1
	echo "$_txt" | grep -F -q "$SHORT_ACK" && SHORT_ACKED=1
	pane_stopped "$_txt" && STOPPED=1
	if pane_busy "$_txt"; then
		BUSY_AFTER=1
	else
		BUSY_AFTER=0
	fi
	if [ "$DONE_LONG" -eq 1 ] && [ "$SHORT_ACKED" -eq 1 ]; then
		save_pane "$RUN/pane-final.txt"
		break
	fi
	if [ "$STOPPED" -eq 1 ] && [ "$DONE_LONG" -eq 0 ]; then
		save_pane "$RUN/pane-interrupt.txt"
		break
	fi
	sleep 2
	_i=$((_i + 2))
done
[ -f "$RUN/pane-final.txt" ] || save_pane "$RUN/pane-final.txt"
[ -f "$RUN/pane-interrupt.txt" ] || true

# 打断瞬间：sidecar 里 Working 消失且出现 SHORT_TOKEN、未出现 DONE-LONG
python3 - "$RUN" "$SHORT_TOKEN" "$END_MARK" "$SHORT_ACK" <<'PY' >"$EVID/tick-scan.txt" || true
import pathlib, sys
run = pathlib.Path(sys.argv[1])
short = sys.argv[2]
end = sys.argv[3]
ack = sys.argv[4]
hits = []
for p in sorted(run.glob("pane-tick-*.txt"), key=lambda x: x.name):
    t = p.read_text(encoding="utf-8", errors="replace")
    hits.append({
        "file": p.name,
        "working": "Working" in t,
        "short": short in t,
        "ack": ack in t,
        "done": end in t,
        "stopped": ("Stopped" in t) or ("interrupted" in t.lower()) or ("enter send now" in t.lower()),
        "n12": "N-12" in t,
    })
working_then_gone = False
interrupt_tick = ""
saw_working = False
for h in hits:
    if h["working"]:
        saw_working = True
    if saw_working and (not h["working"]) and h["short"] and (not h["done"]):
        working_then_gone = True
        interrupt_tick = h["file"]
        break
print(f"tick_n={len(hits)}")
print(f"working_then_gone={str(working_then_gone).lower()}")
print(f"interrupt_tick={interrupt_tick}")
for h in hits:
    print(
        f"{h['file']} working={h['working']} short={h['short']} ack={h['ack']} "
        f"done={h['done']} stopped={h['stopped']} n12={h['n12']}"
    )
PY
copy_out "$EVID/tick-scan.txt"
# copy a few ticks into evid
_n=0
for _f in "$RUN"/pane-tick-*.txt; do
	[ -f "$_f" ] || continue
	_n=$((_n + 1))
	if [ "$_n" -le 8 ] || [ "$_n" -ge 40 ]; then
		copy_out "$_f"
	fi
done
INT_TICK=$(awk -F= '/^working_then_gone=/{print $2; exit}' "$EVID/tick-scan.txt" 2>/dev/null || echo false)
INT_FILE=$(awk -F= '/^interrupt_tick=/{print $2; exit}' "$EVID/tick-scan.txt" 2>/dev/null || true)
if [ -n "$INT_FILE" ] && [ -f "$RUN/$INT_FILE" ]; then
	cp "$RUN/$INT_FILE" "$EVID/pane-interrupt-tick.txt" 2>/dev/null || true
fi

FINAL=$(cat "$RUN/pane-final.txt" 2>/dev/null || true)
echo "$FINAL" | grep -F -q "$END_MARK" && DONE_LONG=1
echo "$FINAL" | grep -F -q "$SHORT_ACK" && SHORT_ACKED=1
echo "$FINAL" | grep -F -q "$SHORT_TOKEN" && SHORT_SEEN=1
pane_stopped "$FINAL" && STOPPED=1
if pane_busy "$FINAL"; then
	BUSY_AFTER=1
else
	BUSY_AFTER=0
fi
COUNT_AFTER=$(echo "$FINAL" | grep -c 'N-12' || true)

SEND1_N=$(awk -F= '/^enter_count=/{print $2; exit}' "$EVID/enter-send1.txt" 2>/dev/null || echo 0)
[ -n "$SEND1_N" ] || SEND1_N=0
BUSY2="unknown"
[ -f "$EVID/send2-busy.txt" ] && BUSY2=$(awk -F= '/^send2_while_busy=/{print $2; exit}' "$EVID/send2-busy.txt")

{
	echo "busy_reached=yes"
	echo "send2_token_visible=$INJECT2"
	echo "send2_while_busy=$BUSY2"
	echo "enter_count_send1=$SEND1_N"
	echo "enter_count_send2=$ENTER_N"
	echo "events_max_attempts=$EV_ATT"
	echo "events_max_attempt_index=$EV_IDX"
	echo "deferred_busy_n=$DEFERRED"
	echo "done_long=$DONE_LONG"
	echo "short_acked=$SHORT_ACKED"
	echo "short_seen=$SHORT_SEEN"
	echo "stopped=$STOPPED"
	echo "busy_after=$BUSY_AFTER"
	echo "n12_before=$COUNT_BEFORE"
	echo "n12_after=$COUNT_AFTER"
	echo "working_then_gone=$INT_TICK"
} | tee "$EVID/verdict-inputs.txt"

RETRY2=0
if [ "$ENTER_N" -ge 2 ] 2>/dev/null; then
	RETRY2=1
fi
if [ "$EV_ATT" -ge 2 ] 2>/dev/null; then
	RETRY2=1
fi
if [ "$EV_IDX" -ge 2 ] 2>/dev/null; then
	RETRY2=1
fi

INTERRUPTED=0
if [ "$DONE_LONG" -eq 0 ] && [ "$SHORT_SEEN" -eq 1 ]; then
	if [ "$STOPPED" -eq 1 ] || [ "$SHORT_ACKED" -eq 1 ]; then
		INTERRUPTED=1
	fi
fi

if [ "$INTERRUPTED" -eq 1 ] && [ "$RETRY2" -eq 1 ]; then
	fail_repro "busy-inject double-enter interrupt enter_count_send2=$ENTER_N events_attempts=$EV_ATT done_long=0 short_acked=$SHORT_ACKED stopped=$STOPPED"
fi

if [ "$INTERRUPTED" -eq 1 ] && [ "$RETRY2" -ne 1 ]; then
	unjudgeable "turn looked interrupted but send2 Enter<$ENTER_N (not proven double-enter) events_attempts=$EV_ATT"
fi

if [ "$DONE_LONG" -eq 1 ] && [ "$SHORT_ACKED" -eq 1 ]; then
	if [ "$RETRY2" -eq 1 ]; then
		pass_ok "retry fired on send2 but long turn completed and short ack at boundary enter_count_send2=$ENTER_N"
	fi
	pass_ok "long turn completed and short token consumed at boundary enter_count_send1=$SEND1_N enter_count_send2=$ENTER_N retry2=$RETRY2"
fi

if [ "$RETRY2" -ne 1 ]; then
	unjudgeable "retry path not triggered on busy inject enter_count_send2=$ENTER_N enter_count_send1=$SEND1_N events_max_attempts=$EV_ATT (honest: no second Enter during in-progress turn)"
fi

unjudgeable "retry executed but outcome ambiguous enter_count_send2=$ENTER_N done_long=$DONE_LONG short_acked=$SHORT_ACKED stopped=$STOPPED"
