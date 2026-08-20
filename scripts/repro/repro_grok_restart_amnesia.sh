#!/bin/sh
# //!
# //! purpose: 固化 grok 整队 restart 失忆（先红）——隔离 workspace 起 1 个
# //!          grok subscription 席，送暗号等到 ACK，restart 后探屏。
# //!          暗号仍在屏上 = 恢复成立 exit 0；屏上无暗号且 grok 已起来 = 复现
# //!          exit 1；起不了席 / 登录墙 / 超预算 / 高载下的红 = exit 2。
# //! contract:
# //!   provides:
# //!     - name: grok-restart-amnesia-repro
# //!       what: 一条命令，退出码即判据；修前须红、修后须绿；同一装置验收
# //!   requires:
# //!     - name: team-agent-0.5.66
# //!       what: TEAM_AGENT_BIN 或默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!     - name: grok-subscription
# //!       what: 本机 grok 已登录；本装置不读 auth.json / 会话正文
# //! boundary:
# //!   - 三态：0 通过(恢复) / 1 复现(失忆) / 2 不可判；禁止把 2 折进 1
# //!   - 隔离临时 workspace + 私有 caller socket；不继承 raw TMUX/TMUX_PANE
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的 PID
# //!   - 不读 .env；不读 ~/.grok/sessions 正文（只 ls 文件名）
# //!   - 观测是重启后、追问前的屏；模型后来搜文件找到暗号 ≠ resume 绿
# //!   - 预算：单次 300s；超时 exit 2。高载窗下的红记 2（只产假红不产假绿）
# //! maturity: wired
#
# 用法: sh repro_grok_restart_amnesia.sh
# 环境: TEAM_AGENT_BIN  KEEP_TMP=1  PROMPT_TIMEOUT  ACK_TIMEOUT  RESTART_PROMPT_TIMEOUT

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
TA="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
TMUX_BIN="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-t105-repro-$$"
TEAM_ID="t105r$$"
AGENT_ID="w1"
TOKEN="X7-t105-$(date +%H%M%S)-$$"
CALLER="$RUN/caller.sock"
WS="$RUN/ws"
TEAMDIR="$WS/t105team"
STAMP=$(date +%Y%m%dT%H%M%S)
EVID="$NODE/runs/$STAMP"
PROMPT_TIMEOUT="${PROMPT_TIMEOUT:-90}"
ACK_TIMEOUT="${ACK_TIMEOUT:-120}"
RESTART_PROMPT_TIMEOUT="${RESTART_PROMPT_TIMEOUT:-90}"
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

unjudgeable() { echo "UNJUDGEABLE: $*" >&2; finish 2; }
fail_repro() { echo "REPRODUCED: $*" >&2; finish 1; }
pass_resume() { echo "RESUME_OK: $*" >&2; finish 0; }

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
	uptime | awk -F'load averages?: ' '{print $2}' | awk '{print $1}'
}

copy_out() {
	[ -n "${1:-}" ] && [ -e "$1" ] || return 0
	mkdir -p "$EVID"
	cp "$1" "$EVID/$(basename "$1")" 2>/dev/null || true
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
		echo "token=$TOKEN"
		echo "team_id=$TEAM_ID"
		echo "ws=$WS"
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
		"$TMUX_BIN" -S "$SOCK" kill-server >/dev/null 2>&1 || true
	fi
	if [ -S "$CALLER" ]; then
		"$TMUX_BIN" -S "$CALLER" kill-server >/dev/null 2>&1 || true
	fi
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(narrow_ps "$_p" | awk 'NR>1 {print $5; exit}')
			case "$_comm" in
			tmux|team-agent|grok|bash|sh)
				kill -TERM "$_p" >/dev/null 2>&1 || true
				;;
			*)
				echo "cleanup: skip pid $_p comm=${_comm:-unknown} (not in allow-list)" >&2
				;;
			esac
		fi
	done
	if [ "${KEEP_TMP:-0}" != "1" ] && [ -d "$RUN" ]; then
		# copy remaining evidence then drop isolation dir
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
    spawned = False
    wr = take(data, "worker_readiness")
    if isinstance(wr, dict) and "all_workers_spawned" in wr:
        spawned = bool(wr["all_workers_spawned"])
    ok = take(data, "ok")
    reason = take(data, "reason") or ""
    attach = ""
    ac = take(data, "attach_commands")
    if isinstance(ac, list) and ac:
        attach = str(ac[0])
    session = take(data, "session_name") or ""
    print(f"ok={ok}")
    print(f"spawned={str(spawned).lower()}")
    print(f"reason={reason}")
    print(f"attach={attach}")
    print(f"session={session}")
elif kind == "restart":
    ok = take(data, "ok")
    status = take(data, "status") or ""
    attach = ""
    ac = take(data, "attach_commands")
    if isinstance(ac, list) and ac:
        attach = str(ac[0])
    session = take(data, "session_name") or ""
    cpid = ""
    coord = take(data, "coordinator")
    if isinstance(coord, dict) and "pid" in coord:
        cpid = str(coord["pid"])
    print(f"ok={ok}")
    print(f"status={status}")
    print(f"attach={attach}")
    print(f"session={session}")
    print(f"coordinator_pid={cpid}")
elif kind == "send":
    ok = take(data, "ok")
    print(f"ok={ok}")
else:
    raise SystemExit("UNKNOWN_KIND")
PY
}

parse_attach() {
	# stdin: "tmux -S /path attach -t name" or "tmux -L name attach -t sess"
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

dump_state_keys() {
	python3 - "$1" "$AGENT_ID" <<'PY'
import json, sys, pathlib
p = pathlib.Path(sys.argv[1])
aid = sys.argv[2]
if not p.is_file():
    print("state=ABSENT")
    raise SystemExit(0)
data = json.loads(p.read_text(encoding="utf-8"))
if not isinstance(data, dict):
    print("state=NOT_OBJECT")
    raise SystemExit(0)
if "agents" not in data:
    print("agents_key=ABSENT")
    raise SystemExit(0)
agents = data["agents"]
if not isinstance(agents, dict) or aid not in agents:
    print(f"agent={aid} ABSENT")
    raise SystemExit(0)
ag = agents[aid]
if not isinstance(ag, dict):
    print("agent_not_object")
    raise SystemExit(0)
for k in ("provider", "session_id", "_pending_session_id", "capture_state", "captured_via", "first_send_at", "status"):
    if k in ag:
        print(f"{k}={ag[k]!r} present=True")
    else:
        print(f"{k}=ABSENT present=False")
PY
}

filter_events() {
	python3 - "$1" "$2" <<'PY'
import json, sys, pathlib
src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
out = []
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
        if name not in ("restart.resume_decision", "provider.worker.spawn_argv"):
            continue
        row = {"event": name}
        for k in ("agent_id", "worker_id", "provider", "decision", "has_session_id",
                  "has_first_send_at", "allow_fresh", "expected_session_id",
                  "session_id_in_argv", "argv_has_resume", "argv_has_session_id",
                  "session_id", "ts"):
            if k in ev:
                row[k] = ev[k]
        if "argv" in ev and isinstance(ev["argv"], list):
            flags = [x for x in ev["argv"] if isinstance(x, str) and x.startswith("-")]
            row["argv_flags"] = flags
            row["argv_has_resume"] = "--resume" in ev["argv"] or "-r" in ev["argv"]
            row["argv_has_session_id"] = "--session-id" in ev["argv"] or "-s" in ev["argv"]
        out.append(row)
dst.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"events_filtered_n={len(out)}")
for row in out:
    if row["event"] == "restart.resume_decision":
        dec = row["decision"] if "decision" in row else "ABSENT"
        print(f"resume_decision={dec}")
PY
}

list_grok_session_names() {
	python3 - "$1" "$2" <<'PY'
import os, sys, pathlib, urllib.parse
ws = pathlib.Path(sys.argv[1]).resolve()
outp = pathlib.Path(sys.argv[2])
enc = urllib.parse.quote(str(ws), safe="")
root = pathlib.Path.home() / ".grok" / "sessions" / enc
lines = [f"encoded_cwd={enc}", f"sessions_dir={root}", f"sessions_dir_exists={root.is_dir()}"]
names = []
if root.is_dir():
    for child in sorted(root.iterdir()):
        names.append(child.name)
        if child.is_dir():
            files = ",".join(sorted(p.name for p in child.iterdir()))
            lines.append(f"uuid_dir={child.name} files={files}")
        else:
            lines.append(f"file={child.name}")
lines.append("uuid_count=" + str(sum(1 for child in root.iterdir() if child.is_dir()) if root.is_dir() else 0))
lines.append("names=" + ",".join(names))
text = "\n".join(lines) + "\n"
outp.write_text(text, encoding="utf-8")
sys.stdout.write(text)
PY
}

wait_file() {
	_path=$1
	_n=$2
	_i=0
	while [ "$_i" -lt "$_n" ]; do
		[ -f "$_path" ] && return 0
		sleep 1
		_i=$((_i + 1))
	done
	return 1
}

pane_text() {
	"$TMUX_BIN" -S "$SOCK" capture-pane -p -t "$TARGET" -S - -E - 2>/dev/null || true
}

save_pane() {
	_dest=$1
	pane_text >"$_dest" 2>/dev/null || true
	copy_out "$_dest"
}

has_login_wall() {
	_txt=$1
	echo "$_txt" | grep -Eiq 'please log in|not authenticated|grok login|sign in to xai|folder is not trusted|do you trust this folder|untrusted folder' && return 0
	return 1
}

wait_prompt() {
	_secs=$1
	_label=$2
	_i=0
	while [ "$_i" -lt "$_secs" ]; do
		_txt=$(pane_text)
		if has_login_wall "$_txt"; then
			save_pane "$RUN/pane-${_label}-loginwall.txt"
			unjudgeable "login/trust wall on pane during $_label"
		fi
		if echo "$_txt" | grep -q '❯' && ! echo "$_txt" | grep -Eq 'Starting session|Starting…|Starting...'; then
			save_pane "$RUN/pane-${_label}-prompt.txt"
			return 0
		fi
		sleep 2
		_i=$((_i + 2))
	done
	save_pane "$RUN/pane-${_label}-timeout.txt"
	return 1
}

wait_ack() {
	_secs=$1
	_i=0
	while [ "$_i" -lt "$_secs" ]; do
		_txt=$(pane_text)
		if echo "$_txt" | grep -F -q "PONG-$TOKEN"; then
			save_pane "$RUN/pane-ack.txt"
			return 0
		fi
		if has_login_wall "$_txt"; then
			save_pane "$RUN/pane-ack-loginwall.txt"
			unjudgeable "login/trust wall while waiting ACK"
		fi
		sleep 2
		_i=$((_i + 2))
	done
	save_pane "$RUN/pane-ack-timeout.txt"
	return 1
}

find_target() {
	_sess=$1
	_win=$("$TMUX_BIN" -S "$SOCK" list-windows -t "$_sess" -F '#{window_name}' 2>/dev/null || true)
	echo "windows=$_win" >&2
	echo "$_win" | grep -qx "$AGENT_ID" && echo "$_sess:$AGENT_ID" && return 0
	# first window as fallback, but mark
	_first=$(echo "$_win" | awk 'NF{print; exit}')
	if [ -n "$_first" ]; then
		echo "$_sess:$_first"
		return 0
	fi
	return 1
}

# ── preflight ──────────────────────────────────────────────
need_cmd python3
[ -x "$TA" ] || unjudgeable "team-agent not executable: $TA"
[ -x "$TMUX_BIN" ] || unjudgeable "tmux not executable: $TMUX_BIN"

# do not inherit production tmux identity
unset TMUX
unset TMUX_PANE

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$EVID"

BIN_MD5=$(md5 -q "$TA")
BIN_MTIME=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S' "$TA")
BIN_VER=$("$TA" --version 2>/dev/null | head -n 1)
NCPU=$(sysctl -n hw.ncpu 2>/dev/null || echo 0)
LOAD_BEFORE=$(load1)
PROT_BEFORE=$(protected_present)
[ "$PROT_BEFORE" -eq 3 ] || unjudgeable "protected sockets present=$PROT_BEFORE (need 3); refusing to run"

{
	echo "probe=grok-restart-amnesia"
	echo "bin_path=$TA"
	echo "bin_md5=$BIN_MD5"
	echo "bin_mtime=$BIN_MTIME"
	echo "bin_version=$BIN_VER"
	echo "load_before=$LOAD_BEFORE"
	echo "ncpu=$NCPU"
	echo "protected_before=$PROT_BEFORE"
	echo "token=$TOKEN"
	echo "team_id=$TEAM_ID"
	echo "ws=$WS"
	echo "budget_sec=$BUDGET_SEC"
} | tee "$EVID/gauge.txt"

echo "load_before=$LOAD_BEFORE ncpu=$NCPU"

cat >"$TEAMDIR/TEAM.md" <<EOF
---
name: ${TEAM_ID}
objective: isolated grok restart-amnesia repro (one worker)
dangerous_auto_approve: false
fast: false
---

Team config only.
EOF

cat >"$TEAMDIR/agents/${AGENT_ID}.md" <<EOF
---
name: ${AGENT_ID}
role: Grok Repro Worker
provider: grok
auth_mode: subscription
dangerously_skip_permissions: true
model: grok-4.6
tools:
  - mcp_team
  - provider_builtin
---

You are a disposable repro worker. Reply to the leader exactly as asked. Do not search the workspace for secrets. Do not call get_team_status.
EOF

# private caller tmux (not a protected name)
"$TMUX_BIN" -S "$CALLER" new-session -d -s t105caller -c "$WS" -- /bin/bash
CALLER_SERVER_PID=$("$TMUX_BIN" -S "$CALLER" display-message -p -t t105caller '#{pid}' 2>/dev/null || true)
CALLER_PANE_PID=$("$TMUX_BIN" -S "$CALLER" display-message -p -t t105caller '#{pane_pid}' 2>/dev/null || true)
record_pid "$CALLER_SERVER_PID"
record_pid "$CALLER_PANE_PID"
echo "caller_sock=$CALLER"
echo "caller_server_pid=$CALLER_SERVER_PID"
echo "caller_pane_pid=$CALLER_PANE_PID"
narrow_ps "$CALLER_SERVER_PID" | tee "$EVID/caller-ps.txt"

# quick-start inside caller so TMUX identity is the private socket
QS_RC_FILE="$RUN/qs.rc"
"$TMUX_BIN" -S "$CALLER" new-window -t t105caller -n launch -c "$WS" -- /bin/sh -c "
PATH='/opt/homebrew/bin:/usr/bin:/bin'
export PATH
export GROK_FOLDER_TRUST=0
unset TMUX_PANE
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

if [ "$QS_INFO" = "PARSE_FAIL" ] || [ "$QS_INFO" = "FILE_ABSENT" ]; then
	unjudgeable "quick-start json unreadable rc=$QS_RC stderr=$(tr '\n' ' ' <"$RUN/qs.err" | awk '{print substr($0,1,400)}')"
fi

# leader unbound is expected from private pane; not "could not start grok"
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

# wait session
_i=0
while [ "$_i" -lt 30 ]; do
	"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "tmux session $SESSION missing on $SOCK"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window on $SESSION"
echo "target=$TARGET"
echo "target=$TARGET" >>"$EVID/gauge.txt"

if ! wait_prompt "$PROMPT_TIMEOUT" before_send; then
	unjudgeable "grok prompt not seen within ${PROMPT_TIMEOUT}s before send"
fi

# send secret; do not write token into workspace files
# 指令里不出现连续子串 PONG-$TOKEN，避免用户气泡把 ACK 等待假绿
SEND_MSG="记住暗号 ${TOKEN}。收到后只回复一行：单词 PONG、一个连字符、然后是该暗号。不要搜索工作区，不要调用工具。"
PATH="/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
GROK_FOLDER_TRUST=0 \
"$TA" send "$AGENT_ID" "$SEND_MSG" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send.json" 2>"$RUN/send.err"
SEND_RC=$?
copy_out "$RUN/send.json"
copy_out "$RUN/send.err"
echo "send_rc=$SEND_RC"
SEND_INFO=$(json_field "$RUN/send.json" send 2>/dev/null || echo "ok=missing")
echo "$SEND_INFO" | tee "$EVID/send.parsed"
SEND_OK=$(echo "$SEND_INFO" | awk -F= '/^ok=/{print $2; exit}')
if [ "$SEND_RC" -ne 0 ] || [ "$SEND_OK" != "True" ] && [ "$SEND_OK" != "true" ]; then
	# coordinator persist may still be ok:true with other casing
	if [ "$SEND_OK" != "True" ] && [ "$SEND_OK" != "true" ]; then
		unjudgeable "send failed rc=$SEND_RC ok=$SEND_OK"
	fi
fi

if ! wait_ack "$ACK_TIMEOUT"; then
	unjudgeable "PONG-$TOKEN not on pane within ${ACK_TIMEOUT}s (setup failed, not a fake-red)"
fi
echo "ack=yes"

# truth sources while still up
STATE="$WS/.team/runtime/state.json"
dump_state_keys "$STATE" | tee "$EVID/state-after-ack.txt"
EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events.jsonl"
	filter_events "$RUN/events.jsonl" "$EVID/events-after-ack.json"
fi
list_grok_session_names "$WS" "$EVID/grok-sessions-after-ack.txt" | tee "$RUN/grok-sessions-after-ack.txt"

# shutdown then restart without --allow-fresh
PATH="/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
"$TA" shutdown --workspace "$WS" --team "$TEAM_ID" --keep-logs --json >"$RUN/shutdown1.json" 2>"$RUN/shutdown1.err"
SD_RC=$?
copy_out "$RUN/shutdown1.json"
echo "shutdown_rc=$SD_RC"
[ "$SD_RC" -eq 0 ] || unjudgeable "shutdown failed rc=$SD_RC"

PATH="/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
GROK_FOLDER_TRUST=0 \
"$TA" restart "$WS" --team "$TEAM_ID" --json >"$RUN/restart.json" 2>"$RUN/restart.err"
RS_RC=$?
copy_out "$RUN/restart.json"
copy_out "$RUN/restart.err"
echo "restart_rc=$RS_RC"
RS_INFO=$(json_field "$RUN/restart.json" restart 2>"$RUN/restart.parse.err" || echo "PARSE_FAIL")
echo "$RS_INFO" | tee "$EVID/restart.parsed"
RS_OK=$(echo "$RS_INFO" | awk -F= '/^ok=/{print $2; exit}')
RS_STATUS=$(echo "$RS_INFO" | awk -F= '/^status=/{print $2; exit}')
RS_ATTACH=$(echo "$RS_INFO" | awk -F= '/^attach=/{sub(/^attach=/,""); print; exit}')
RS_SESS=$(echo "$RS_INFO" | awk -F= '/^session=/{print $2; exit}')
RS_CPID=$(echo "$RS_INFO" | awk -F= '/^coordinator_pid=/{print $2; exit}')
record_pid "$RS_CPID"
if [ "$RS_RC" -ne 0 ] && [ "$RS_OK" != "True" ] && [ "$RS_OK" != "true" ]; then
	unjudgeable "restart failed rc=$RS_RC status=$RS_STATUS"
fi
if [ -n "$RS_ATTACH" ]; then
	PARSED=$(printf '%s\n' "$RS_ATTACH" | parse_attach)
	_ns=$(echo "$PARSED" | awk -F= '/^sock=/{print $2; exit}')
	[ -n "$_ns" ] && SOCK=$_ns
fi
[ -n "$RS_SESS" ] && SESSION=$RS_SESS
if is_protected_sock "$SOCK"; then
	unjudgeable "restart attach socket is protected: $SOCK"
fi

_i=0
while [ "$_i" -lt 30 ]; do
	"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "session missing after restart"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window after restart"
echo "target_after=$TARGET"

# anti-vacuous: grok must actually come up after restart before judging absence
if ! wait_prompt "$RESTART_PROMPT_TIMEOUT" after_restart; then
	unjudgeable "grok prompt not seen after restart within ${RESTART_PROMPT_TIMEOUT}s (cannot judge amnesia)"
fi

save_pane "$RUN/pane-after-restart.txt"
AFTER=$(cat "$RUN/pane-after-restart.txt" 2>/dev/null || true)
echo "$AFTER" >"$EVID/pane-after-restart.txt"

EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events-after-restart.jsonl"
	filter_events "$EVENTS" "$EVID/events-after-restart.json" | tee "$EVID/events-summary.txt"
fi
dump_state_keys "$STATE" | tee "$EVID/state-after-restart.txt"
list_grok_session_names "$WS" "$EVID/grok-sessions-after-restart.txt" | tee "$RUN/grok-sessions-after-restart.txt"

# 观测：重启后、追问前。不问模型。搜文件找到 ≠ 屏上有暗号。
if echo "$AFTER" | grep -F -q "$TOKEN"; then
	pass_resume "secret still on pane after restart (resume appears to have kept context)"
fi
fail_repro "secret $TOKEN absent from pane after restart (prompt visible; amnesia)"
