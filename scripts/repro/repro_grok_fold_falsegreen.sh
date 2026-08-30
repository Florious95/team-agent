#!/bin/sh
# //!
# //! purpose: 固化悬案B grok 折叠假绿（先红）——隔离 grok 席注入 ≥25 行
# //!          长贴。框架判 delivered（events/submit-verification）而
# //!          composer 仍有 grok `[Pasted: N lines]` 占位符 ⇒ exit 1。
# //!          样本#5：首次 Enter 后框短暂空、约 20s 占位符重现，计入红。
# //! contract:
# //!   provides:
# //!     - name: grok-fold-falsegreen-repro
# //!       what: 一条命令，退出码即判据；修前须红、修后须绿；同一装置验收
# //!   requires:
# //!     - name: team-agent-0.5.66
# //!       what: 默认 ~/.team-agent/runtime/0.5.66/bin/team-agent
# //!             md5 b81c70816ff504d44f1d4a041373c84f；TEAM_AGENT_BIN 可覆盖
# //!     - name: grok-subscription
# //!       what: 本机真实 HOME 已登录；不读 .env / 会话正文 / 不打印 proxy 值
# //! boundary:
# //!   - 三态：0 无假绿 / 1 复现(delivered∧占位符) / 2 不可判；禁止把 2 折进 1
# //!   - 真实 HOME，禁止 export HOME=
# //!   - qs 到达看 all_workers_spawned，不看 rc=0
# //!   - 隔离临时 workspace + 私有 caller socket；不继承 raw TMUX/TMUX_PANE
# //!   - 受保护 socket 只核存在：ta-a9fd5b7defbd / ta-a0afa5f9c7f6 / ta-b7cc1c640ccf
# //!   - ps 窄字段 pid,ppid,etime,stat,comm；只杀本脚本记录的精确 PID
# //!   - 复现不出如实 exit 2；高载窗红记 2（假绿确定性全绿仍有效）
# //!   - 预算：单次 300s。额度：只发 1 条长贴，不等模型回复
# //!   - POSIX：禁 bash 进程替换；proxy 只报 present/len
# //! maturity: wired
#
# 用法: sh repro_grok_fold_falsegreen.sh
# 环境: TEAM_AGENT_BIN  KEEP_TMP=1  PROMPT_TIMEOUT  WATCH_SEC  BUDGET_SEC

set -u

NODE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SRC_BIN="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="b81c70816ff504d44f1d4a041373c84f"
REAL_TMUX="${TMUX_BIN:-/opt/homebrew/bin/tmux}"
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-t129-repro-$$"
TEAM_ID="t129r$$"
AGENT_ID="w1"
TOKEN="T129-$$"
CALLER="$RUN/caller.sock"
WS="$RUN/ws"
TEAMDIR="$WS/t129team"
STAMP=$(date +%Y%m%dT%H%M%S)
EVID="$NODE/runs/$STAMP"
PROMPT_TIMEOUT="${PROMPT_TIMEOUT:-90}"
WATCH_SEC="${WATCH_SEC:-25}"
BUDGET_SEC="${BUDGET_SEC:-300}"
LINE_N="${LINE_N:-79}"
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
TOTAL_CM=0

unjudgeable() { echo "UNJUDGEABLE: $*" >&2; finish 2; }
fail_repro() { echo "REPRODUCED: $*" >&2; finish 1; }
pass_ok() { echo "NO_FALSEGREEN: $*" >&2; finish 0; }

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
			# 合取已落盘（events delivered ∧ pane 含占位符）不是超时型假红
			if [ "${SAW_PLACEHOLDER_AFTER_DELIVERED:-0}" -eq 1 ]; then
				echo "note=high-load window load1=$LOAD_BEFORE ncpu=$NCPU; durable delivered∧placeholder still red" >&2
			else
				echo "UNJUDGEABLE: high-load window (load1=$LOAD_BEFORE ncpu=$NCPU); red would be false-red" >&2
				_rc=2
				echo "verdict_rc=$_rc"
			fi
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
		"$REAL_TMUX" -S "$SOCK" kill-server >/dev/null 2>&1 || true
	fi
	if [ -S "$CALLER" ]; then
		"$REAL_TMUX" -S "$CALLER" kill-server >/dev/null 2>&1 || true
	fi
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(comm_base "$_p")
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
	sleep 1
	for _p in $PIDS; do
		if pid_alive "$_p"; then
			_comm=$(comm_base "$_p")
			case "$_comm" in
			tmux|team-agent|grok|bash|sh)
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
			find "$WS/.team" -name 'submit-verification.json' -exec cp {} "$EVID/runtime/submit-verification.json" \; 2>/dev/null || true
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
    print(f"delivered_key={'yes' if 'delivered' in data else 'no'}")
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
            "submit_verification",
        ):
            if k in ev:
                row[k] = ev[k]
        out.append(row)
dst.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"inject_events_n={len(out)}")
n_del = 0
n_arm = 0
n_unv = 0
for row in out:
    extra = ""
    if "message_id" in row:
        extra += f" id={row['message_id']}"
    if "ts" in row:
        extra += f" ts={row['ts']}"
    if "verification" in row:
        extra += f" verification={row['verification']}"
    if "submit_verification" in row:
        extra += f" submit_verification={row['submit_verification']}"
    print(f"ev={row['event']}{extra}")
    if row["event"] == "message.delivered":
        n_del += 1
    if row["event"] == "turn_open.armed_after_inject":
        n_arm += 1
    if row["event"] == "send.unverified":
        n_unv += 1
print(f"delivered_n={n_del}")
print(f"armed_after_inject_n={n_arm}")
print(f"unverified_n={n_unv}")
PY
}

summarize_submit_verification() {
	python3 - "$1" <<'PY'
import json, sys, pathlib
p = pathlib.Path(sys.argv[1])
if not p.is_file():
    print("sv_file=ABSENT")
    raise SystemExit(0)
raw = p.read_text(encoding="utf-8", errors="replace").strip()
if not raw:
    print("sv_file=EMPTY")
    raise SystemExit(0)
start = raw.find("{")
if start < 0:
    print("sv_file=NO_JSON")
    raise SystemExit(0)
try:
    data = json.loads(raw[start:])
except json.JSONDecodeError:
    print("sv_file=PARSE_FAIL")
    raise SystemExit(0)
if not isinstance(data, dict):
    print("sv_file=NOT_OBJECT")
    raise SystemExit(0)
print("sv_file=PRESENT")
for k in (
    "consumed",
    "verification",
    "submit_verification",
    "submit_verified",
    "inject_verification",
    "attempts",
    "ok",
    "status",
    "message_id",
):
    if k in data:
        print(f"sv_{k}={data[k]!r} present=True")
    else:
        print(f"sv_{k}=ABSENT present=False")
PY
}

count_enters() {
	python3 - "$1" <<'PY'
import sys, pathlib
log = pathlib.Path(sys.argv[1])
enters = 0
if log.is_file():
    for ln in log.read_text(encoding="utf-8", errors="replace").splitlines():
        enters += ln.count("Enter") + ln.count("C-m")
print(enters)
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

pane_mode_info() {
	_in=$("$REAL_TMUX" -S "$SOCK" display-message -p -t "$TARGET" '#{pane_in_mode}' 2>/dev/null || echo "?")
	_md=$("$REAL_TMUX" -S "$SOCK" display-message -p -t "$TARGET" '#{pane_mode}' 2>/dev/null || echo "?")
	echo "pane_in_mode=${_in}"
	echo "pane_mode=${_md}"
}

has_login_wall() {
	_txt=$1
	echo "$_txt" | grep -Eiq 'please log in|not authenticated|grok login|sign in to xai|folder is not trusted|do you trust this folder|untrusted folder' && return 0
	return 1
}

has_grok_chrome() {
	_txt=$1
	echo "$_txt" | grep -q '❯' || return 1
	echo "$_txt" | grep -Eq 'Grok 4|Grok 3|Starting session' || return 0
	echo "$_txt" | grep -q 'Grok' && return 0
	return 0
}

# grok 折叠：样本#1 `[Pasted: N lines]`；现网 Grok 4.6 还出 `[Pasted: N KB]`
# 不是 claude `[Pasted text #N`
GROK_PASTE_RE='\[Pasted: [0-9]+([.][0-9]+)? (lines|KB|kB|MB|B)\]'

has_grok_placeholder() {
	_txt=$1
	echo "$_txt" | grep -Eq "$GROK_PASTE_RE" && return 0
	return 1
}

placeholder_lit() {
	python3 - "$1" <<'PY'
import re, sys
text = sys.argv[1]
ms = re.findall(r"\[Pasted: [0-9]+(?:[.][0-9]+)? (?:lines|KB|kB|MB|B)\]", text)
print(ms[-1] if ms else "")
PY
}

tooth_placeholder_detector() {
	_pos_lines='> [Pasted: 27 lines]
Enter:send  Esc:cancel'
	_pos_kb='│ ❯ [Pasted: 13 KB]                                                          │
 Enter:send'
	_neg='❯
Grok 4.6
ready'
	echo "$_pos_lines" | grep -Eq "$GROK_PASTE_RE" || return 1
	echo "$_pos_kb" | grep -Eq "$GROK_PASTE_RE" || return 1
	echo "$_neg" | grep -Eq "$GROK_PASTE_RE" && return 1
	return 0
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
		if echo "$_txt" | grep -q '❯' && ! echo "$_txt" | grep -Eq 'Starting session|Starting…|Starting...'; then
			# 空闲 composer：尽量等 MCP 转完再贴，对齐样本#1/#5 的空闲席
			if echo "$_txt" | grep -Eq '[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]'; then
				sleep 2
				_i=$((_i + 2))
				continue
			fi
			save_pane "$RUN/pane-${_label}-prompt.txt"
			return 0
		fi
		sleep 2
		_i=$((_i + 2))
	done
	save_pane "$RUN/pane-${_label}-timeout.txt"
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

find_events() {
	find "$WS/.team" -name events.jsonl 2>/dev/null | awk 'NR==1{print}'
}

find_sv() {
	find "$WS/.team" -name submit-verification.json 2>/dev/null | awk 'NR==1{print}'
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

build_payload() {
	python3 - "$TOKEN" "$LINE_N" <<'PY'
import sys
token = sys.argv[1]
n = int(sys.argv[2])
if n < 25:
    raise SystemExit("LINE_N_TOO_SMALL")
lines = [
    f"PR19-REPRO token={token}",
    "Do not use tools. Do not search the workspace.",
    "If a reply is required, output the single word IGNORE.",
]
i = 1
pad = "W" * 160
while len(lines) < n - 1:
    lines.append(f"L{i:02d} {token} {pad}")
    i += 1
lines.append(f"[team-agent-token:{token}]")
text = "\n".join(lines) + "\n"
print(f"payload_lines={len(lines)}", file=sys.stderr)
sys.stdout.write(text)
PY
}

# ── preflight ──────────────────────────────────────────────
need_cmd python3
need_cmd md5
[ -x "$SRC_BIN" ] || unjudgeable "team-agent not executable: $SRC_BIN"
[ -x "$REAL_TMUX" ] || unjudgeable "tmux not executable: $REAL_TMUX"

if ! tooth_placeholder_detector; then
	unjudgeable "placeholder detector tooth failed (positive [Pasted: 27 lines] or negative empty-composer)"
fi

unset TMUX
unset TMUX_PANE

mkdir -p "$RUN" "$WS" "$TEAMDIR/agents" "$EVID" /tmp/ta-t129-bin
cp "$SRC_BIN" /tmp/ta-t129-bin/team-agent
chmod +x /tmp/ta-t129-bin/team-agent
TA=/tmp/ta-t129-bin/team-agent
BIN_MD5=$(md5 -q "$TA")
BIN_MTIME=$(stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%S' "$TA")
BIN_SIZE=$(stat -f '%z' "$TA")
BIN_VER=$("$TA" --version 2>/dev/null | head -n 1)
if [ -z "${TEAM_AGENT_BIN:-}" ] && [ "$BIN_MD5" != "$EXPECT_MD5" ]; then
	unjudgeable "default runtime md5=$BIN_MD5 expected=$EXPECT_MD5"
fi
echo "gauge_md5_selfcheck expected=$EXPECT_MD5 got=$BIN_MD5 src=$SRC_BIN copy=$TA"

install_tmux_wrapper
SAFE_PATH="$RUN/bin:/Users/alauda/.grok/proxy/bin:/Users/alauda/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin"
export PATH="$SAFE_PATH"

NCPU=$(/usr/sbin/sysctl -n hw.ncpu 2>/dev/null || echo 0)
LOAD_BEFORE=$(load1)
PROT_BEFORE=$(protected_present)
[ "$PROT_BEFORE" -eq 3 ] || unjudgeable "protected sockets present=$PROT_BEFORE (need 3); refusing to run"

{
	echo "probe=grok-fold-falsegreen"
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
	echo "line_n=$LINE_N"
	echo "watch_sec=$WATCH_SEC"
	echo "home_isolated=false"
	echo "budget_sec=$BUDGET_SEC"
	proxy_gauge
} | tee "$EVID/gauge.txt"

echo "load_before=$LOAD_BEFORE ncpu=$NCPU"

cat >"$TEAMDIR/TEAM.md" <<EOF
---
name: ${TEAM_ID}
objective: isolated grok fold false-green repro (one worker)
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

You are a disposable repro worker. Do not search the workspace. Do not call get_team_status. If a filler paste arrives, reply IGNORE or stay idle.
EOF

"$REAL_TMUX" -S "$CALLER" new-session -d -s t129caller -c "$WS" -- /bin/bash
CALLER_SERVER_PID=$("$REAL_TMUX" -S "$CALLER" display-message -p -t t129caller '#{pid}' 2>/dev/null || true)
CALLER_PANE_PID=$("$REAL_TMUX" -S "$CALLER" display-message -p -t t129caller '#{pane_pid}' 2>/dev/null || true)
record_pid "$CALLER_SERVER_PID"
record_pid "$CALLER_PANE_PID"
echo "caller_sock=$CALLER"
echo "caller_server_pid=$CALLER_SERVER_PID"
echo "caller_pane_pid=$CALLER_PANE_PID"
narrow_ps "$CALLER_SERVER_PID" | tee "$EVID/caller-ps.txt"

QS_RC_FILE="$RUN/qs.rc"
"$REAL_TMUX" -S "$CALLER" new-window -t t129caller -n launch -c "$WS" -- /bin/sh -c "
PATH='$SAFE_PATH'
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
	unjudgeable "quick-start json unreadable rc=$QS_RC"
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
	"$REAL_TMUX" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null && break
	sleep 1
	_i=$((_i + 1))
done
"$REAL_TMUX" -S "$SOCK" has-session -t "$SESSION" 2>/dev/null || unjudgeable "tmux session $SESSION missing on $SOCK"

TARGET=$(find_target "$SESSION") || unjudgeable "cannot find worker window on $SESSION"
echo "target=$TARGET"
echo "target=$TARGET" >>"$EVID/gauge.txt"

if ! wait_prompt "$PROMPT_TIMEOUT" before_send; then
	unjudgeable "grok prompt not seen within ${PROMPT_TIMEOUT}s before send"
fi

# 第三半：发送前 composer 不得被占位符检测器命中，否则尺子恒绿/恒红
PRE=$(pane_text)
if has_grok_placeholder "$PRE"; then
	save_pane "$RUN/pane-pre-placeholder-unexpected.txt"
	unjudgeable "pre-send composer already has grok [Pasted: N lines]; detector/scene contaminated"
fi
pane_mode_info | tee "$EVID/pane-mode-before.txt"
save_pane "$RUN/pane-before-send.txt"

PAYLOAD_FILE="$RUN/payload.txt"
build_payload >"$PAYLOAD_FILE" 2>"$RUN/payload.meta"
copy_out "$RUN/payload.meta"
PAYLOAD_LINES=$(awk -F= '/^payload_lines=/{print $2; exit}' "$RUN/payload.meta")
echo "payload_lines=$PAYLOAD_LINES"
[ "${PAYLOAD_LINES:-0}" -ge 25 ] || unjudgeable "payload_lines=$PAYLOAD_LINES < 25"

# 额度节制：只发这一条。正文不出现 ACK 等待串。
# send 期间并行抓屏：折叠占位符窗可能短于 send 返回。
mkdir -p "$RUN/live"
: >"$RUN/capture.alive"
(
	_i=0
	while [ -f "$RUN/capture.alive" ]; do
		_ts=$(python3 -c 'import time; print("%.3f" % time.time())')
		pane_text >"$RUN/live/pane-${_i}.txt" 2>/dev/null || true
		printf 'ts=%s\n' "$_ts" >"$RUN/live/pane-${_i}.meta"
		_i=$((_i + 1))
		sleep 0.15
	done
) &
CAP_PID=$!
record_pid "$CAP_PID"

SEND_MSG=$(cat "$PAYLOAD_FILE")
PATH="$SAFE_PATH" \
GROK_FOLDER_TRUST=0 \
"$TA" send "$AGENT_ID" "$SEND_MSG" --workspace "$WS" --team "$TEAM_ID" --json >"$RUN/send.json" 2>"$RUN/send.err"
SEND_RC=$?
rm -f "$RUN/capture.alive"
wait "$CAP_PID" 2>/dev/null || true
copy_out "$RUN/send.json"
copy_out "$RUN/send.err"
echo "send_rc=$SEND_RC"
SEND_INFO=$(json_field "$RUN/send.json" send 2>/dev/null || echo "ok=missing")
echo "$SEND_INFO" | tee "$EVID/send.parsed"
SEND_OK=$(echo "$SEND_INFO" | awk -F= '/^ok=/{print $2; exit}')
SEND_DELIVERED=$(echo "$SEND_INFO" | awk -F= '/^delivered=/{print $2; exit}')
if [ "$SEND_OK" != "True" ] && [ "$SEND_OK" != "true" ]; then
	unjudgeable "send failed rc=$SEND_RC ok=$SEND_OK"
fi

save_pane "$RUN/pane-after-send.txt"
pane_mode_info | tee "$EVID/pane-mode-after-send.txt"
copy_out "$RUN/tmux-keys.log"

SAW_PLACEHOLDER=0
SAW_PLACEHOLDER_AFTER_DELIVERED=0
SAW_EMPTY_AFTER_DELIVERED=0
SAW_REAPPEAR=0
PLACEHOLDER_LIT=""
DELIVERED_EVENTS=0
ARMED_N=0
UNVERIFIED_N=0
CLI_DELIVERED=0
case "$SEND_DELIVERED" in
True|true) CLI_DELIVERED=1 ;;
esac

# send 返回瞬间先读 events，再跟占位符对齐（避免 tick0 先看屏后补事件漏同拍）
EVENTS=$(find_events)
if [ -n "$EVENTS" ]; then
	cp "$EVENTS" "$RUN/events.jsonl"
	filter_inject_events "$RUN/events.jsonl" "$RUN/events-inject.json" | tee "$RUN/events-inject.summary" || true
	DELIVERED_EVENTS=$(awk -F= '/^delivered_n=/{print $2; exit}' "$RUN/events-inject.summary")
	ARMED_N=$(awk -F= '/^armed_after_inject_n=/{print $2; exit}' "$RUN/events-inject.summary")
	UNVERIFIED_N=$(awk -F= '/^unverified_n=/{print $2; exit}' "$RUN/events-inject.summary")
fi
SV=$(find_sv)
if [ -n "$SV" ]; then
	cp "$SV" "$RUN/submit-verification.json"
	summarize_submit_verification "$RUN/submit-verification.json" | tee "$RUN/sv.summary"
fi
fw_now() {
	if [ "$DELIVERED_EVENTS" -ge 1 ] || [ "$CLI_DELIVERED" -eq 1 ]; then
		return 0
	fi
	if [ -f "$RUN/sv.summary" ]; then
		if grep -q "sv_submit_verified=True present=True" "$RUN/sv.summary" || grep -q "sv_submit_verified=true present=True" "$RUN/sv.summary"; then
			return 0
		fi
		if grep -q "sv_consumed=True present=True" "$RUN/sv.summary" || grep -q "sv_consumed=true present=True" "$RUN/sv.summary"; then
			return 0
		fi
	fi
	return 1
}

AFTER=$(cat "$RUN/pane-after-send.txt" 2>/dev/null || true)
if has_grok_placeholder "$AFTER"; then
	SAW_PLACEHOLDER=1
	PLACEHOLDER_LIT=$(placeholder_lit "$AFTER")
	if fw_now; then
		SAW_PLACEHOLDER_AFTER_DELIVERED=1
	fi
elif fw_now; then
	# 样本#5：delivered 后框先空，稍后占位符重现
	SAW_EMPTY_AFTER_DELIVERED=1
fi

# 并行抓屏 vs delivered ts：占位符帧时间 >= delivered 事件秒 ⇒ 合取
LIVE_OVERLAP=$(python3 - "$RUN/live" "$RUN/events.jsonl" <<'PY'
import json, re, sys, pathlib
from datetime import datetime, timezone
live = pathlib.Path(sys.argv[1])
evp = pathlib.Path(sys.argv[2])
pat = re.compile(r"\[Pasted: [0-9]+(?:[.][0-9]+)? (?:lines|KB|kB|MB|B)\]")

def parse_ts(s):
    s = str(s).replace("Z", "+00:00")
    try:
        dt = datetime.fromisoformat(s)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()

delivered_epoch = None
if evp.is_file():
    for ln in evp.read_text(encoding="utf-8", errors="replace").splitlines():
        ln = ln.strip()
        if not ln.startswith("{"):
            continue
        try:
            ev = json.loads(ln)
        except json.JSONDecodeError:
            continue
        if not isinstance(ev, dict) or "event" not in ev:
            continue
        if ev["event"] != "message.delivered":
            continue
        if "ts" not in ev:
            continue
        delivered_epoch = parse_ts(ev["ts"])
        break
placeholder_any = False
placeholder_lits = []
overlap = False
frames = 0
ph_frames = 0
if live.is_dir():
    files = sorted(
        live.glob("pane-*.txt"),
        key=lambda p: int(p.stem.split("-")[-1]) if p.stem.split("-")[-1].isdigit() else 0,
    )
    for fp in files:
        frames += 1
        text = fp.read_text(encoding="utf-8", errors="replace")
        m = pat.search(text)
        if not m:
            continue
        placeholder_any = True
        ph_frames += 1
        placeholder_lits.append(m.group(0))
        meta = fp.with_suffix(".meta")
        cap_epoch = fp.stat().st_mtime
        if meta.is_file():
            for ml in meta.read_text(encoding="utf-8", errors="replace").splitlines():
                if ml.startswith("ts="):
                    try:
                        cap_epoch = float(ml.split("=", 1)[1])
                    except ValueError:
                        pass
        # 占位符帧不早于 delivered（容 0.5s 钟偏）：合取才红
        if delivered_epoch is not None and cap_epoch + 0.5 >= delivered_epoch:
            overlap = True
print(f"live_frames={frames}")
print(f"live_placeholder_frames={ph_frames}")
print(f"live_placeholder_any={str(placeholder_any).lower()}")
print(f"live_overlap={str(overlap).lower()}")
print(f"live_lit={placeholder_lits[-1] if placeholder_lits else ''}")
print(f"live_delivered_epoch={delivered_epoch if delivered_epoch is not None else 'ABSENT'}")
PY
)
echo "$LIVE_OVERLAP" | tee "$EVID/live-overlap.txt"
LIVE_ANY=$(echo "$LIVE_OVERLAP" | awk -F= '/^live_placeholder_any=/{print $2; exit}')
LIVE_OL=$(echo "$LIVE_OVERLAP" | awk -F= '/^live_overlap=/{print $2; exit}')
LIVE_LIT=$(echo "$LIVE_OVERLAP" | awk -F= '/^live_lit=/{sub(/^live_lit=/,""); print; exit}')
if [ "$LIVE_ANY" = "true" ]; then
	SAW_PLACEHOLDER=1
	[ -n "$LIVE_LIT" ] && PLACEHOLDER_LIT=$LIVE_LIT
fi
if [ "$LIVE_OL" = "true" ]; then
	SAW_PLACEHOLDER_AFTER_DELIVERED=1
fi
if [ -d "$RUN/live" ]; then
	mkdir -p "$EVID/live"
	cp "$RUN/live"/pane-*.txt "$EVID/live/" 2>/dev/null || true
fi

_tick=0
WATCH_START=$(date +%s)
_watch_end=$((WATCH_START + WATCH_SEC))
while [ "$(date +%s)" -le "$_watch_end" ]; do
	budget_hit && unjudgeable "budget ${BUDGET_SEC}s hit during watch"
	EVENTS=$(find_events)
	if [ -n "$EVENTS" ]; then
		cp "$EVENTS" "$RUN/events.jsonl"
		filter_inject_events "$RUN/events.jsonl" "$RUN/events-inject.json" >"$RUN/events-inject.summary" || true
		DELIVERED_EVENTS=$(awk -F= '/^delivered_n=/{print $2; exit}' "$RUN/events-inject.summary")
		ARMED_N=$(awk -F= '/^armed_after_inject_n=/{print $2; exit}' "$RUN/events-inject.summary")
		UNVERIFIED_N=$(awk -F= '/^unverified_n=/{print $2; exit}' "$RUN/events-inject.summary")
	fi
	SV=$(find_sv)
	if [ -n "$SV" ]; then
		cp "$SV" "$RUN/submit-verification.json"
		summarize_submit_verification "$RUN/submit-verification.json" >"$RUN/sv.summary"
	fi
	_txt=$(pane_text)
	printf '%s\n' "$_txt" >"$RUN/pane-tick-${_tick}.txt"
	copy_out "$RUN/pane-tick-${_tick}.txt"
	pane_mode_info >"$RUN/pane-mode-tick-${_tick}.txt"
	copy_out "$RUN/pane-mode-tick-${_tick}.txt"
	_in=$(awk -F= '/^pane_in_mode=/{print $2; exit}' "$RUN/pane-mode-tick-${_tick}.txt")
	if [ "$_in" != "0" ] && [ "$_in" != "false" ] && [ "$_in" != "?" ]; then
		echo "note=pane_in_mode=$_in at tick=$_tick (copy-mode family; not 悬案B)" >&2
	fi
	_fw=0
	if [ "$DELIVERED_EVENTS" -ge 1 ] || [ "$CLI_DELIVERED" -eq 1 ]; then
		_fw=1
	fi
	if [ -f "$RUN/sv.summary" ]; then
		if grep -q "sv_consumed=True present=True" "$RUN/sv.summary" || grep -q "sv_consumed=true present=True" "$RUN/sv.summary"; then
			_fw=1
		fi
		if grep -q "sv_submit_verified=True present=True" "$RUN/sv.summary" || grep -q "sv_submit_verified=true present=True" "$RUN/sv.summary"; then
			_fw=1
		fi
	fi
	if has_grok_placeholder "$_txt"; then
		SAW_PLACEHOLDER=1
		PLACEHOLDER_LIT=$(placeholder_lit "$_txt")
		if [ "$_fw" -eq 1 ]; then
			SAW_PLACEHOLDER_AFTER_DELIVERED=1
			if [ "$SAW_EMPTY_AFTER_DELIVERED" -eq 1 ]; then
				SAW_REAPPEAR=1
			fi
		fi
	else
		if [ "$_fw" -eq 1 ]; then
			SAW_EMPTY_AFTER_DELIVERED=1
		fi
	fi
	if [ "$SAW_PLACEHOLDER_AFTER_DELIVERED" -eq 1 ]; then
		echo "note=caught delivered∧placeholder at tick=$_tick lit=$PLACEHOLDER_LIT reappear=$SAW_REAPPEAR"
		break
	fi
	_tick=$((_tick + 1))
	# 前 8s 0.2s 采样（样本#5 空框后再现可能短于 1s）；其后 1s
	_elapsed_watch=$(($(date +%s) - WATCH_START))
	if [ "$_elapsed_watch" -lt 8 ]; then
		sleep 0.2
	else
		sleep 1
	fi
done

copy_out "$RUN/events.jsonl"
copy_out "$RUN/events-inject.json"
copy_out "$RUN/events-inject.summary"
copy_out "$RUN/submit-verification.json"
copy_out "$RUN/sv.summary"
copy_out "$RUN/tmux-keys.log"
save_pane "$RUN/pane-final.txt"
pane_mode_info | tee "$EVID/pane-mode-final.txt"

if [ -f "$RUN/events-inject.json" ]; then
	cp "$RUN/events-inject.json" "$EVID/events-inject.json"
fi
if [ -f "$RUN/events-inject.summary" ]; then
	cp "$RUN/events-inject.summary" "$EVID/events-inject.summary"
fi

TOTAL_CM=$(count_enters "$RUN/tmux-keys.log")
echo "enter_count=$TOTAL_CM"

SV_CONSUMED_TRUE=0
SV_VERIFIED_TRUE=0
if [ -f "$RUN/sv.summary" ]; then
	if grep -q "sv_consumed=True present=True" "$RUN/sv.summary" || grep -q "sv_consumed=true present=True" "$RUN/sv.summary"; then
		SV_CONSUMED_TRUE=1
	fi
	if grep -q "sv_submit_verified=True present=True" "$RUN/sv.summary" || grep -q "sv_submit_verified=true present=True" "$RUN/sv.summary"; then
		SV_VERIFIED_TRUE=1
	fi
fi

FRAMEWORK_DELIVERED=0
if [ "$DELIVERED_EVENTS" -ge 1 ] || [ "$CLI_DELIVERED" -eq 1 ] || [ "$SV_CONSUMED_TRUE" -eq 1 ] || [ "$SV_VERIFIED_TRUE" -eq 1 ]; then
	FRAMEWORK_DELIVERED=1
fi

{
	echo "payload_lines=$PAYLOAD_LINES"
	echo "send_rc=$SEND_RC"
	echo "send_ok=$SEND_OK"
	echo "cli_delivered=$CLI_DELIVERED"
	echo "delivered_events=$DELIVERED_EVENTS"
	echo "armed_after_inject_n=$ARMED_N"
	echo "unverified_n=$UNVERIFIED_N"
	echo "sv_consumed_true=$SV_CONSUMED_TRUE"
	echo "sv_submit_verified_true=$SV_VERIFIED_TRUE"
	echo "framework_delivered=$FRAMEWORK_DELIVERED"
	echo "saw_placeholder=$SAW_PLACEHOLDER"
	echo "placeholder_lit=$PLACEHOLDER_LIT"
	echo "saw_placeholder_after_delivered=$SAW_PLACEHOLDER_AFTER_DELIVERED"
	echo "saw_empty_after_delivered=$SAW_EMPTY_AFTER_DELIVERED"
	echo "saw_reappear=$SAW_REAPPEAR"
	echo "enter_count=$TOTAL_CM"
} | tee "$EVID/verdict-inputs.txt"

# 到达性
if [ "$TOTAL_CM" -lt 1 ] && [ "$ARMED_N" -lt 1 ] && [ "$FRAMEWORK_DELIVERED" -eq 0 ]; then
	unjudgeable "inject did not reach Enter/arm/delivered (enter_count=$TOTAL_CM armed=$ARMED_N)"
fi

FINAL_MODE=$(awk -F= '/^pane_in_mode=/{print $2; exit}' "$EVID/pane-mode-final.txt")
if [ "$FINAL_MODE" != "0" ] && [ "$FINAL_MODE" != "false" ] && [ "$SAW_PLACEHOLDER" -eq 1 ]; then
	unjudgeable "pane_in_mode=$FINAL_MODE at final capture; not 悬案B (non-copy-mode) family"
fi

# 假绿：框架判 delivered 且占位符仍在（含 20s 重现）
if [ "$FRAMEWORK_DELIVERED" -eq 1 ] && [ "$SAW_PLACEHOLDER_AFTER_DELIVERED" -eq 1 ]; then
	fail_repro "delivered while grok placeholder still in composer lit=$PLACEHOLDER_LIT reappear=$SAW_REAPPEAR enter_count=$TOTAL_CM delivered_events=$DELIVERED_EVENTS armed=$ARMED_N"
fi

if [ "$SAW_PLACEHOLDER" -eq 0 ]; then
	unjudgeable "grok [Pasted: N lines|N KB] never appeared; fold not triggered (cannot claim 悬案B). delivered=$FRAMEWORK_DELIVERED enter_count=$TOTAL_CM"
fi

# Gone 分支仍是 EnterSentWithoutPlaceholderCheck 但占位符没粘滞：
# 先红未做成（Enter 未被吞），⛔ 不得报 0 冒充修后绿。
if [ -f "$RUN/sv.summary" ] && grep -q 'EnterSentWithoutPlaceholderCheck' "$RUN/sv.summary"; then
	unjudgeable "Gone path still EnterSentWithoutPlaceholderCheck but placeholder did not stick after delivered (lit=$PLACEHOLDER_LIT reappear=$SAW_REAPPEAR). not a post-fix green"
fi

# 折叠发生了，框架未在占位符仍在时宣告 delivered（修后 unverified / 改判定）
pass_ok "fold seen lit=$PLACEHOLDER_LIT framework_delivered=$FRAMEWORK_DELIVERED placeholder_after_delivered=$SAW_PLACEHOLDER_AFTER_DELIVERED unverified=$UNVERIFIED_N enter_count=$TOTAL_CM"
