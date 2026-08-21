#!/bin/sh
# //!
# //! purpose: 固化 cursor 整队 restart 会话回路——隔离 workspace 起 1 个
# //!          cursor_agent 席，restart 后看是否 --resume。
# //!          捕获未做成 = exit 2 不可判 resume 绿；有 session_id 却 fresh = exit 1；
# //!          有 --resume = exit 0。
# //! contract:
# //!   provides:
# //!     - name: cursor-restart-amnesia-repro
# //!       what: 一条命令，退出码即判据；U-01/U-02 通后才有绿
# //!   requires:
# //!     - name: team-agent-bin
# //!       what: TEAM_AGENT_BIN 指向被测二进制（不 exec target/ 正在被写的文件）
# //! boundary:
# //!   - 三态：0 恢复 / 1 有 sid 却失忆 / 2 不可判（捕获未做成/起不了席/超时）
# //!   - 隔离临时 workspace；不继承 raw TMUX/TMUX_PANE
# //!   - 不读 .env；不读 ~/.cursor/chats 正文
# //!   - 永不发无值 --resume / --continue
# //!   - 不跑 team-agent claude
# //! maturity: wired
#
# 用法: TEAM_AGENT_BIN=/path/to/copied-bin sh scripts/repro/repro_cursor_restart_amnesia.sh

set -u

TA="${TEAM_AGENT_BIN:-}"
if [ -z "$TA" ] || [ ! -x "$TA" ]; then
  echo "UNJUDGEABLE: set TEAM_AGENT_BIN to a copied binary" >&2
  exit 2
fi
TMP_ROOT="${TEAM_AGENT_TEST_TMP:-/private/tmp}"
[ -d "$TMP_ROOT" ] || TMP_ROOT=/tmp
RUN="$TMP_ROOT/ta-d115-cursor-repro-$$"
TEAM_ID="c115r$$"
AGENT_ID="w1"
unset TMUX TMUX_PANE

cleanup() {
  if [ -z "${KEEP_TMP:-}" ]; then
    "$TA" shutdown --workspace "$WS" --yes >/dev/null 2>&1 || true
    rm -rf "$RUN"
  fi
}
trap cleanup EXIT

mkdir -p "$RUN/ws/agents" "$RUN/home"
WS="$RUN/ws"
export HOME="$RUN/home"

cat > "$WS/TEAM.md" <<EOF
---
name: $TEAM_ID
objective: cursor resume probe.
provider: cursor_agent
dangerously_skip_permissions: true
---

Team.
EOF
cat > "$WS/agents/${AGENT_ID}.md" <<EOF
---
name: $AGENT_ID
role: Cursor Writer
provider: cursor_agent
model: sonnet-4-thinking
auth_mode: subscription
dangerously_skip_permissions: true
tools:
  - mcp_team
---

Worker.
EOF

{
  echo "bin=$TA"
  md5 "$TA" 2>/dev/null || md5sum "$TA"
  stat -f "mtime=%Sm" -t "%Y-%m-%d %H:%M:%S" "$TA" 2>/dev/null || true
} > "$RUN/gauge.txt"

if ! "$TA" quick-start "$WS" --workspace "$WS" --team-id "$TEAM_ID" --name "$TEAM_ID" --yes --no-display --json \
  >"$RUN/qs.json" 2>"$RUN/qs.err"; then
  echo "UNJUDGEABLE: quick-start failed" >&2
  cat "$RUN/qs.err" >&2
  exit 2
fi

python3 - "$WS" "$AGENT_ID" "$RUN/capture.txt" <<'PY'
import json, pathlib, sys
ws, agent_id, out = pathlib.Path(sys.argv[1]), sys.argv[2], pathlib.Path(sys.argv[3])
state_path = ws / ".team/runtime/state.json"
if not state_path.is_file():
    print("UNJUDGEABLE: no state.json")
    sys.exit(2)
state = json.loads(state_path.read_text())
agent = (state.get("agents") or {}).get(agent_id) or {}
if not agent:
    for team in (state.get("teams") or {}).values():
        agent = (team.get("agents") or {}).get(agent_id) or {}
        if agent:
            break
sid = agent.get("session_id")
pending = agent.get("_pending_session_id")
out.write(f"session_id={sid}\npending={pending}\n")
print(f"session_id={sid!r} pending={pending!r}")
if not sid:
    print("UNJUDGEABLE: capture not done (U-01/U-02); resume green forbidden")
    sys.exit(2)
sys.exit(0)
PY
cap_rc=$?
if [ "$cap_rc" -eq 2 ]; then
  echo "capture missing — honest gap, not resume green" >&2
  exit 2
fi

if ! "$TA" restart --workspace "$WS" --yes --json >"$RUN/restart.json" 2>"$RUN/restart.err"; then
  echo "UNJUDGEABLE: restart failed" >&2
  cat "$RUN/restart.err" >&2
  exit 2
fi

python3 - "$WS" <<'PY'
import json, pathlib, sys
ws = pathlib.Path(sys.argv[1])
path = ws / ".team/logs/events.jsonl"
has_resume = False
empty_resume = False
if path.is_file():
    for line in path.read_text().splitlines():
        try:
            ev = json.loads(line)
        except Exception:
            continue
        if ev.get("event") != "provider.worker.spawn_argv":
            continue
        if ev.get("source") != "restart":
            continue
        argv = ev.get("argv") or []
        if "--continue" in argv:
            print("FAIL: --continue on restart")
            sys.exit(1)
        if "--resume" in argv:
            i = argv.index("--resume")
            nxt = argv[i + 1] if i + 1 < len(argv) else ""
            if not nxt or nxt.startswith("-"):
                empty_resume = True
            else:
                has_resume = True
if empty_resume:
    print("FAIL: empty --resume")
    sys.exit(1)
if has_resume:
    print("resume argv present")
    sys.exit(0)
print("FAIL: session_id present but restart did not --resume")
sys.exit(1)
PY
