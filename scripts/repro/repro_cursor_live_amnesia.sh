#!/bin/sh
# //!
# //! purpose: 固化 cursor 活体 restart 失忆（先红）——真实 HOME + 隔离
# //!          workspace 起 1 个 cursor_agent subscription 席，发短暗号，核
# //!          chats 唯一新目录 vs state.session_id，整队 restart 后探屏。
# //!          sid 未捕获或暗号丢 = exit 1(红=复现)；两者都在且 --resume =
# //!          exit 0；起不了席 / 登录墙 / 无唯一新目录 / 超预算 / 高载红 = 2。
# //! contract:
# //!   provides:
# //!     - name: cursor-live-amnesia-repro
# //!       what: 一条命令，退出码即判据；修前须红、修后须绿；同一装置验收
# //!   requires:
# //!     - name: team-agent-0.5.66
# //!       what: 默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!             md5 efcfa29c193ce1721aa062b5e0577593；TEAM_AGENT_BIN 可覆盖
# //!     - name: cursor-subscription
# //!       what: 本机真实 HOME 已登录；本装置不读 .env / chats 正文 / 不打印 proxy 值
# //! boundary:
# //!   - 三态：0 通过(恢复) / 1 复现(失忆) / 2 不可判；禁止把 2 折进 1
# //!   - 真实 HOME，禁止 export HOME=（官方 repro_cursor_restart_amnesia.sh 坑）
# //!   - qs 到达看 all_workers_spawned，不看 rc=0（unbound 不折 2）
# //!   - 隔离临时 workspace + 私有 caller socket；不继承 raw TMUX/TMUX_PANE
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的精确 PID
# //!   - 不读 ~/.cursor/chats 正文（只 ls 名 + test -f marker）
# //!   - 观测是重启后、追问前的屏；搜磁盘找到暗号 ≠ resume 绿
# //!   - 预算：单次 300s；超时 exit 2。高载窗下的红记 2
# //!   - POSIX：禁 bash 进程替换；proxy 只报 present/len
# //! maturity: wired
#
# 用法: sh repro_cursor_live_amnesia.sh
# 环境: TEAM_AGENT_BIN  KEEP_TMP=1  PROMPT_TIMEOUT  CIPHER_TIMEOUT  RESTART_PROMPT_TIMEOUT

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="efcfa29c193ce1721aa062b5e0577593"
TMUX_BIN="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-t122-repro-$$"
TEAM_ID="t122r$$"
AGENT_ID="w1"
TOKEN="T122-$$"
CALLER="$RUN/caller.sock"
WS="$RUN/ws"
TEAMDIR="$WS/t122team"
STAMP=$(date +%Y%m%dT%H%M%S)
EVID="$NODE/runs/$STAMP"
PROMPT_TIMEOUT="${PROMPT_TIMEOUT:-90}"
CIPHER_TIMEOUT="${CIPHER_TIMEOUT:-90}"
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
TA=""
BIN_MD5=""

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
	sysctl -n vm.loadavg 2>/dev/null | awk '{gsub(/[{}]/,""); print $1}'
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
		echo "token=$TOKEN"
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
		"$TMUX_BIN" -S "$SOCK" kill-server >/dev/null 2>&1 || true
	fi
	if [ -S "$CALLER" ]; then
		"$TMUX_BIN" -S "$CALLER" kill-server >/dev/null 2>&1 || true
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
                  "session_id", "source", "ts"):
            if k in ev:
                row[k] = ev[k]
        if "argv" in ev and isinstance(ev["argv"], list):
            flags = [x for x in ev["argv"] if isinstance(x, str) and x.startswith("-")]
            row["argv_flags"] = flags
            row["argv_has_resume"] = "--resume" in ev["argv"]
            row["argv_has_continue"] = "--continue" in ev["argv"]
        out.append(row)
dst.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"events_filtered_n={len(out)}")
for row in out:
    if row["event"] == "restart.resume_decision":
        dec = row["decision"] if "decision" in row else "ABSENT"
        print(f"resume_decision={dec}")
    if row["event"] == "provider.worker.spawn_argv":
        src = row["source"] if "source" in row else "?"
        hr = row["argv_has_resume"] if "argv_has_resume" in row else False
        print(f"spawn_argv source={src} argv_has_resume={hr}")
PY
}

list_cursor_chats() {
	python3 - "$1" "$2" <<'PY'
import hashlib, sys, pathlib
ws = pathlib.Path(sys.argv[1]).resolve()
outp = pathlib.Path(sys.argv[2])
digest = hashlib.md5(str(ws).encode("utf-8")).hexdigest()
root = pathlib.Path.home() / ".cursor" / "chats" / digest
lines = [
    f"ws_resolved={ws}",
    f"hex={digest}",
    f"chats_dir={root}",
    f"chats_dir_exists={root.is_dir()}",
]
uuids = []
if root.is_dir():
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            lines.append(f"file={child.name}")
            continue
        uuids.append(child.name)
        names = ",".join(sorted(p.name for p in child.iterdir()))
        store = (child / "store.db").is_file()
        meta = (child / "meta.json").is_file()
        try:
            mt = int(child.stat().st_mtime)
        except OSError:
            mt = -1
        lines.append(
            f"uuid_dir={child.name} mtime={mt} marker_store={store} marker_meta={meta} names={names}"
        )
lines.append("uuid_count=" + str(len(uuids)))
lines.append("uuids=" + ",".join(uuids))
text = "\n".join(lines) + "\n"
outp.write_text(text, encoding="utf-8")
sys.stdout.write(text)
PY
}

new_uuids_vs() {
	python3 - "$1" "$2" <<'PY'
import sys, pathlib
before = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").splitlines()
after = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8", errors="replace").splitlines()

def uuids(lines):
    out = []
    for ln in lines:
        if ln.startswith("uuid_dir="):
            rest = ln[len("uuid_dir="):]
            name = rest.split(" ", 1)[0]
            out.append(name)
    return out

b = set(uuids(before))
a = uuids(after)
new = [u for u in a if u not in b]
print("new_count=" + str(len(new)))
print("new_uuids=" + ",".join(new))
marked = 0
for ln in after:
    if not ln.startswith("uuid_dir="):
        continue
    rest = ln[len("uuid_dir="):]
    name = rest.split(" ", 1)[0]
    if name not in new:
        continue
    if "marker_store=True" in ln or "marker_meta=True" in ln:
        marked += 1
print("new_marked=" + str(marked))
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
	"$TMUX_BIN" -S "$SOCK" capture-pane -p -t "$TARGET" -S - -E - 2>/dev/null || true
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

wait_cipher_and_chats() {
	_secs=$1
	_i=0
	while [ "$_i" -lt "$_secs" ]; do
		budget_hit && return 1
		_txt=$(pane_text)
		if has_login_wall "$_txt"; then
			save_pane "$RUN/pane-cipher-loginwall.txt"
			unjudgeable "login wall while waiting cipher"
		fi
		if echo "$_txt" | grep -F -q "$TOKEN"; then
			# 对照起席前快照：cursor 常在 TUI 起来时就写出 chats/<hex>/<uuid>/meta.json
			# 相对发送前快照会把「起席已有的唯一目录」折成 new_count=0 假不可判
			list_cursor_chats "$WS" "$RUN/chats-after-send.txt" >/dev/null
			DIFF=$(new_uuids_vs "$RUN/chats-before-spawn.txt" "$RUN/chats-after-send.txt")
			echo "$DIFF" >"$RUN/chats-diff.txt"
			_nc=$(echo "$DIFF" | awk -F= '/^new_count=/{print $2; exit}')
			_nm=$(echo "$DIFF" | awk -F= '/^new_marked=/{print $2; exit}')
			if [ "$_nc" = "1" ] && [ "$_nm" = "1" ]; then
				save_pane "$RUN/pane-ack.txt"
				copy_out "$RUN/chats-after-send.txt"
				copy_out "$RUN/chats-diff.txt"
				return 0
			fi
		fi
		sleep 2
		_i=$((_i + 2))
	done
	save_pane "$RUN/pane-ack-timeout.txt"
	list_cursor_chats "$WS" "$RUN/chats-after-send.txt" >/dev/null
	new_uuids_vs "$RUN/chats-before-spawn.txt" "$RUN/chats-after-send.txt" | tee "$RUN/chats-diff.txt"
	copy_out "$RUN/chats-after-send.txt"
	copy_out "$RUN/chats-diff.txt"
	return 1
}

find_target() {
	_sess=$1
	_win=$("$TMUX_BIN" -S "$SOCK" list-windows -t "$_sess" -F '#{window_name}' 2>/dev/null || true)
	echo "windows=$_win" >&2
	echo "$_win" | grep -qx "$AGENT_ID" && echo "$_sess:$AGENT_ID" && return 0
	_first=$(echo "$_win" | awk 'NF{print; exit}')
	if [ -n "$_first" ]; then
		echo "$_sess:$_first"
		return 0
	fi
	return 1
}

# ── preflight ──────────────────────────────────────────────
need_cmd python3
need_cmd md5
[ -x "$SRC_BIN" ] || unjudgeable "team-agent not executable: $SRC_BIN"
[ -x "$TMUX_BIN" ] || unjudgeable "tmux not executable: $TMUX_BIN"
[ -x /Users/alauda/.local/bin/agent ] || unjudgeable "PATH agent missing: /Users/alauda/.local/bin/agent"

unset TMUX
unset TMUX_PANE

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$EVID" /tmp/ta-t122-bin
cp "$SRC_BIN" /tmp/ta-t122-bin/team-agent
chmod +x /tmp/ta-t122-bin/team-agent
TA=/tmp/ta-t122-bin/team-agent
BIN_MD5=$(md5 -q "$TA")
BIN_MTIME=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S' "$TA")
BIN_SIZE=$(stat -f '%z' "$TA")
BIN_VER=$("$TA" --version 2>/dev/null | head -n 1)
if [ -z "${TEAM_AGENT_BIN:-}" ] && [ "$BIN_MD5" != "$EXPECT_MD5" ]; then
	unjudgeable "default runtime md5=$BIN_MD5 expected=$EXPECT_MD5"
fi

NCPU=$(sysctl -n hw.ncpu 2>/dev/null || echo 0)
LOAD_BEFORE=$(load1)
PROT_BEFORE=$(protected_present)
[ "$PROT_BEFORE" -eq 3 ] || unjudgeable "protected sockets present=$PROT_BEFORE (need 3); refusing to run"

{
	echo "probe=cursor-live-amnesia"
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
	echo "token=$TOKEN"
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
objective: isolated cursor live-amnesia repro (one worker)
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

You are a disposable repro worker. Do not search the workspace. Do not call any tools. Do not call get_team_status. If you see a short token, reply with that token only.
EOF

"$TMUX_BIN" -S "$CALLER" new-session -d -s t122caller -c "$WS" -- /bin/bash
CALLER_SERVER_PID=$("$TMUX_BIN" -S "$CALLER" display-message -p -t t122caller '#{pid}' 2>/dev/null || true)
CALLER_PANE_PID=$("$TMUX_BIN" -S "$CALLER" display-message -p -t t122caller '#{pane_pid}' 2>/dev/null || true)
record_pid "$CALLER_SERVER_PID"
record_pid "$CALLER_PANE_PID"
echo "caller_sock=$CALLER"
echo "caller_server_pid=$CALLER_SERVER_PID"
echo "caller_pane_pid=$CALLER_PANE_PID"
narrow_ps "$CALLER_SERVER_PID" | tee "$EVID/caller-ps.txt"

list_cursor_chats "$WS" "$RUN/chats-before-spawn.txt" | tee "$EVID/chats-before-spawn.txt"

QS_RC_FILE="$RUN/qs.rc"
"$TMUX_BIN" -S "$CALLER" new-window -t t122caller -n launch -c "$WS" -- /bin/sh -c "
PATH='/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin'
export PATH
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

if [ "$QS_INFO" = "PARSE_FAIL" ] || [ "$QS_INFO" = "FILE_ABSENT" ] || [ "$QS_INFO" = "SPAWNED_KEY_ABSENT" ]; then
	unjudgeable "quick-start json unreadable rc=$QS_RC info=$QS_INFO"
fi

# leader unbound is expected from private pane; not "could not start cursor"
if [ "$SPAWNED" != "true" ]; then
	unjudgeable "workers not spawned spawned=$SPAWNED rc=$QS_RC reason=$REASON"
fi
echo "note=quick-start may be rc=1 for leader_receiver_unbound; workers spawned so continue (official repro_cursor_restart_amnesia.sh folds this to exit 2)"

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
	"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "tmux session $SESSION missing on $SOCK"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window on $SESSION"
echo "target=$TARGET"
echo "target=$TARGET" >>"$EVID/gauge.txt"

if ! wait_prompt "$PROMPT_TIMEOUT" before_send; then
	unjudgeable "cursor chrome not seen within ${PROMPT_TIMEOUT}s before send"
fi

list_cursor_chats "$WS" "$RUN/chats-before-send.txt" | tee "$EVID/chats-before-send.txt"

# short cipher; 指令里不出现 ACK 等待串；不把 TOKEN 写进 workspace 文件
SEND_MSG="${TOKEN}
Do not call tools. Reply with this token only."
PATH="/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
"$TA" send "$AGENT_ID" "$SEND_MSG" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send.json" 2>"$RUN/send.err"
SEND_RC=$?
copy_out "$RUN/send.json"
copy_out "$RUN/send.err"
echo "send_rc=$SEND_RC"
SEND_INFO=$(json_field "$RUN/send.json" send 2>/dev/null || echo "ok=missing")
echo "$SEND_INFO" | tee "$EVID/send.parsed"
SEND_OK=$(echo "$SEND_INFO" | awk -F= '/^ok=/{print $2; exit}')
if [ "$SEND_OK" != "True" ] && [ "$SEND_OK" != "true" ]; then
	unjudgeable "send failed rc=$SEND_RC ok=$SEND_OK"
fi

if ! wait_cipher_and_chats "$CIPHER_TIMEOUT"; then
	unjudgeable "cipher+unique marked chats dir not observed within ${CIPHER_TIMEOUT}s (setup failed, not a fake-red)"
fi
echo "cipher_and_unique_archive=yes"

STATE="$WS/.team/runtime/state.json"
dump_state_keys "$STATE" | tee "$EVID/state-after-ack.txt"
EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events.jsonl"
	filter_events "$RUN/events.jsonl" "$EVID/events-after-ack.json"
fi
copy_out "$RUN/chats-after-send.txt"
copy_out "$RUN/chats-diff.txt"

SID_LINE=$(awk -F= '/^session_id=/{print; exit}' "$EVID/state-after-ack.txt")
SID_PRESENT=$(awk '/^session_id=/{print; exit}' "$EVID/state-after-ack.txt" | grep -c 'present=True' || true)
NEW_UUID=$(awk -F= '/^new_uuids=/{print $2; exit}' "$RUN/chats-diff.txt")
echo "sid_line=$SID_LINE"
echo "new_uuid=$NEW_UUID"
SID_CAPTURED=0
case "$SID_LINE" in
*"present=False"*)
	unjudgeable "state.session_id key ABSENT (cannot distinguish empty vs missing)"
	;;
esac
case "$SID_LINE" in
*"session_id=None present=True"*|*"session_id='None' present=True"*|*"session_id=\"None\" present=True"*|*"session_id='' present=True"*|*"session_id=\"\" present=True"*)
	SID_CAPTURED=0
	;;
*)
	echo "$SID_LINE" | grep -F -q "$NEW_UUID" && SID_CAPTURED=1
	;;
esac
echo "sid_captured=$SID_CAPTURED"

PATH="/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
"$TA" shutdown --workspace "$WS" --team "$TEAM_ID" --keep-logs --json >"$RUN/shutdown1.json" 2>"$RUN/shutdown1.err"
SD_RC=$?
copy_out "$RUN/shutdown1.json"
echo "shutdown_rc=$SD_RC"
[ "$SD_RC" -eq 0 ] || unjudgeable "shutdown failed rc=$SD_RC"

PATH="/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:$PATH" \
"$TA" restart --workspace "$WS" --team "$TEAM_ID" --yes --json >"$RUN/restart.json" 2>"$RUN/restart.err"
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
	budget_hit && unjudgeable "budget before restart session"
	"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$TMUX_BIN" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "session missing after restart"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window after restart"
echo "target_after=$TARGET"

if ! wait_prompt "$RESTART_PROMPT_TIMEOUT" after_restart; then
	unjudgeable "cursor chrome not seen after restart within ${RESTART_PROMPT_TIMEOUT}s (cannot judge amnesia)"
fi

save_pane "$RUN/pane-after-restart.txt"
AFTER=$(cat "$RUN/pane-after-restart.txt" 2>/dev/null || true)
echo "$AFTER" >"$EVID/pane-after-restart.txt"

EVENTS=$(find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}')
RESUME_DECISION="ABSENT"
ARGV_HAS_RESUME="unknown"
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events-after-restart.jsonl"
	filter_events "$EVENTS" "$EVID/events-after-restart.json" | tee "$EVID/events-summary.txt"
	RESUME_DECISION=$(awk -F= '/^resume_decision=/{print $2; exit}' "$EVID/events-summary.txt")
	[ -n "$RESUME_DECISION" ] || RESUME_DECISION="ABSENT"
	ARGV_HAS_RESUME=$(awk '/spawn_argv source=restart/{print; exit}' "$EVID/events-summary.txt")
fi
dump_state_keys "$STATE" | tee "$EVID/state-after-restart.txt"
list_cursor_chats "$WS" "$EVID/chats-after-restart.txt" | tee "$RUN/chats-after-restart.txt"

CIPHER_AFTER=0
echo "$AFTER" | grep -F -q "$TOKEN" && CIPHER_AFTER=1
echo "cipher_after=$CIPHER_AFTER sid_captured=$SID_CAPTURED resume_decision=$RESUME_DECISION"

# 绿：sid 等于唯一新 uuid 且 restart --resume 且追问前屏上仍有暗号
if [ "$SID_CAPTURED" -eq 1 ] && [ "$CIPHER_AFTER" -eq 1 ] && echo "$ARGV_HAS_RESUME" | grep -Fq 'argv_has_resume=True'; then
	pass_resume "sid captured, restart --resume, secret still on pane"
fi

# 红：唯一 archive 已在，sid 空 或 重启后暗号丢（chrome 已在，齿已执行）
fail_repro "sid_captured=$SID_CAPTURED cipher_after=$CIPHER_AFTER resume_decision=$RESUME_DECISION new_uuid=$NEW_UUID (prompt visible; live amnesia)"
