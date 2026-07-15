#!/bin/sh
set -eu

MARKER="TMUX_SERVER_DEATH_0544_BANG_ADD_AGENT_OUTCOMES"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

contract_check() {
  printf '%s\n' "$MARKER"
  printf '%s\n' "default path starts a private tmux socket and uses send-keys to run real team-agent add-agent"
  printf '%s\n' "success: --role-file --workspace --team private creates a new worker and old session stays alive"
  printf '%s\n' "failure: PATH shim injects tmux spawn failure, rollback is observed, mcp.server_exit/coordinator.session_missing stay absent"
}

usage() {
  cat <<'USAGE'
TMUX_SERVER_DEATH_0544_BANG_ADD_AGENT_OUTCOMES

Usage:
  tools/gate-harness/tmux-server-death-0544-bang-add-agent.sh
  tools/gate-harness/tmux-server-death-0544-bang-add-agent.sh --contract-check

Environment:
  TEAM_AGENT_BIN       path to the team-agent binary; defaults to PATH lookup
  TEAM_AGENT_TEST_TMP  temp root; defaults to TMPDIR or /tmp

The default path is the real gate: it starts a private tmux server, launches a
fake-provider team, injects bare team-agent add-agent commands through tmux
send-keys, then asserts success and failure outcomes.
USAGE
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found on PATH: $1"
}

sq() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

write_role() {
  path=$1
  id=$2
  mkdir -p "$(dirname "$path")"
  cat >"$path" <<ROLE
---
name: $id
role: Fake worker $id
provider: fake
model: fake
auth_mode: subscription
tools:
  - mcp_team
---

Fake worker $id.
ROLE
}

wait_for_code_file() {
  file=$1
  label=$2
  tries=0
  while [ "$tries" -lt 60 ]; do
    [ -s "$file" ] && return 0
    tries=$((tries + 1))
    sleep 1
  done
  die "$label did not finish; missing code file $file"
}

assert_contains() {
  file=$1
  needle=$2
  label=$3
  grep -F "$needle" "$file" >/dev/null 2>&1 || die "$label missing '$needle' in $file"
}

assert_not_contains() {
  file=$1
  needle=$2
  label=$3
  if [ -f "$file" ] && grep -F "$needle" "$file" >/dev/null 2>&1; then
    die "$label unexpectedly contained '$needle' in $file"
  fi
}

run_team_agent() (
  unset TMUX_PANE TEAM_AGENT_LEADER_PANE_ID TEAM_AGENT_LEADER_SESSION_UUID
  unset TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE TEAM_AGENT_LEADER_PROVIDER
  unset TEAM_AGENT_MACHINE_FINGERPRINT TEAM_AGENT_WORKSPACE TEAM_AGENT_TEAM_ID
  unset TEAM_AGENT_OWNER_TEAM_ID TEAM_AGENT_ACTIVE_TEAM TEAM_AGENT_ID
  HOME=$home
  TMPDIR=$tmp
  TEAM_AGENT_TEST_TMP=$test_tmp
  TMUX=$tmux_env
  export HOME TMPDIR TEAM_AGENT_TEST_TMP TMUX
  "$TEAM_AGENT_BIN" "$@"
)

send_bang_add_agent() {
  label=$1
  agent=$2
  role=$3
  path_value=$4
  out=$tmp/$label.out
  err=$tmp/$label.err
  code=$tmp/$label.code
  rm -f "$out" "$err" "$code"

  cmd="unset TMUX_PANE TEAM_AGENT_LEADER_PANE_ID TEAM_AGENT_LEADER_SESSION_UUID TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE TEAM_AGENT_LEADER_PROVIDER TEAM_AGENT_MACHINE_FINGERPRINT TEAM_AGENT_WORKSPACE TEAM_AGENT_TEAM_ID TEAM_AGENT_OWNER_TEAM_ID TEAM_AGENT_ACTIVE_TEAM TEAM_AGENT_ID; HOME=$(sq "$home") TMPDIR=$(sq "$tmp") TEAM_AGENT_TEST_TMP=$(sq "$test_tmp") TMUX=$(sq "$tmux_env") PATH=$(sq "$path_value") $(sq "$TEAM_AGENT_BIN") add-agent $agent --role-file $(sq "$role") --workspace $(sq "$workspace") --team private --no-display --json >$(sq "$out") 2>$(sq "$err"); printf '%s' \"\$?\" >$(sq "$code")"
  tmux -S "$socket" send-keys -t "$leader_session:bang" -l "$cmd"
  tmux -S "$socket" send-keys -t "$leader_session:bang" Enter
  wait_for_code_file "$code" "$label"
}

assert_window_alive() {
  window=$1
  tmux -S "$socket" list-windows -t "$team_session" -F '#{window_name}' \
    | grep -Fx "$window" >/dev/null 2>&1 \
    || die "expected live tmux window $team_session:$window"
}

assert_no_bad_death_events() {
  events=$workspace/.team/logs/events.jsonl
  assert_not_contains "$events" "mcp.server_exit" "pre-existing worker death guard"
  assert_not_contains "$events" "coordinator.session_missing" "pre-existing coordinator death guard"
  assert_not_contains "$events" "stdin_eof" "pre-existing stdin guard"
}

if [ "${1:-}" = "--contract-check" ]; then
  contract_check
  exit 0
fi

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
  usage
  exit 0
fi

[ "$#" -eq 0 ] || die "unknown argument: $1"

require_cmd tmux
real_tmux=$(command -v tmux)
if [ -z "${TEAM_AGENT_BIN:-}" ]; then
  TEAM_AGENT_BIN=$(command -v team-agent 2>/dev/null || true)
fi
[ -n "${TEAM_AGENT_BIN:-}" ] || die "TEAM_AGENT_BIN is required when team-agent is not on PATH"
[ -x "$TEAM_AGENT_BIN" ] || die "TEAM_AGENT_BIN is not executable: $TEAM_AGENT_BIN"

root=${TEAM_AGENT_TEST_TMP:-${TMPDIR:-/tmp}}
mkdir -p "$root"
tmp=$(mktemp -d "$root/ta-0544-bang.XXXXXX")
home=$tmp/home
workspace=$tmp/workspace
team_dir=$tmp/team
test_tmp=$tmp/test-tmp
socket=$tmp/private-tmux.sock
leader_session=ta-0544-bang-leader
tmux_env=$socket,0,0
mkdir -p "$home" "$workspace" "$team_dir/agents" "$test_tmp"

cleanup() {
  status=$?
  if [ -x "${TEAM_AGENT_BIN:-}" ] && [ -d "${workspace:-/nonexistent}" ]; then
    run_team_agent shutdown --workspace "$workspace" --team private --keep-logs --json >/dev/null 2>&1 || true
  fi
  if [ -n "${socket:-}" ]; then
    tmux -S "$socket" kill-server >/dev/null 2>&1 || true
  fi
  if [ -z "${TEAM_AGENT_KEEP_GATE_HARNESS_TMP:-}" ]; then
    rm -rf "$tmp"
  else
    printf 'kept harness tmp: %s\n' "$tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

cat >"$team_dir/TEAM.md" <<'TEAM'
---
name: private
objective: 0.5.44 private-socket bang add-agent gate.
provider: fake
display_backend: none
---

Private socket bang add-agent gate.
TEAM
write_role "$team_dir/agents/worker_a.md" worker_a
write_role "$tmp/worker_b.md" worker_b
write_role "$tmp/worker_fail.md" worker_fail

tmux -S "$socket" new-session -d -s "$leader_session" -n bang sh

quick_code=0
run_team_agent quick-start "$team_dir" --workspace "$workspace" --team-id private --yes --no-display --json >"$tmp/quick.out" 2>"$tmp/quick.err" \
  || quick_code=$?
if [ "$quick_code" -ne 0 ]; then
  assert_contains "$tmp/quick.out" '"all_workers_spawned": true' "degraded quick-start"
fi

team_session=$(tmux -S "$socket" list-sessions -F '#{session_name}' | grep -v "^$leader_session\$" | head -n 1)
[ -n "$team_session" ] || die "team session was not created on private tmux socket"
tmux -S "$socket" has-session -t "$team_session" >/dev/null 2>&1 || die "team session is not alive: $team_session"
assert_window_alive worker_a

send_bang_add_agent success worker_b "$tmp/worker_b.md" "$PATH"
[ "$(cat "$tmp/success.code")" = "0" ] || die "success add-agent failed: stdout=$(cat "$tmp/success.out") stderr=$(cat "$tmp/success.err")"
assert_contains "$workspace/.team/runtime/state.json" '"worker_b"' "success state"
assert_window_alive worker_a
assert_window_alive worker_b

shim_dir=$tmp/path-shim
mkdir -p "$shim_dir"
cat >"$shim_dir/tmux" <<SHIM
#!/bin/sh
for arg in "\$@"; do
  case "\$arg" in
    new-window|split-window)
      echo "injected tmux spawn failure for 0544 bang add-agent harness" >&2
      exit 42
      ;;
  esac
done
exec $(sq "$real_tmux") "\$@"
SHIM
chmod +x "$shim_dir/tmux"

send_bang_add_agent failure worker_fail "$tmp/worker_fail.md" "$shim_dir:$PATH"
[ "$(cat "$tmp/failure.code")" != "0" ] || die "failure add-agent unexpectedly succeeded"
assert_contains "$tmp/failure.out" "injected tmux spawn failure" "failure stdout"
assert_not_contains "$workspace/.team/runtime/state.json" '"worker_fail"' "rollback state"
spec_path=$(find "$workspace/.team/runtime" -name team.spec.yaml -print | head -n 1)
[ -n "$spec_path" ] || die "runtime team.spec.yaml not found"
assert_not_contains "$spec_path" 'worker_fail' "rollback spec"
assert_contains "$workspace/.team/logs/events.jsonl" "add_agent.rollback" "rollback event"
assert_contains "$workspace/.team/logs/events.jsonl" "worker_fail" "rollback event agent"
assert_window_alive worker_a
assert_window_alive worker_b
assert_no_bad_death_events

run_team_agent status --workspace "$workspace" --team private --json >"$tmp/status.out" 2>"$tmp/status.err" \
  || die "status failed after harness: stdout=$(cat "$tmp/status.out") stderr=$(cat "$tmp/status.err")"

printf '%s\n' "$MARKER"
printf '%s\n' "success: worker_b created via tmux send-keys team-agent add-agent on private socket"
printf '%s\n' "failure: worker_fail rolled back after PATH shim tmux spawn failure; add_agent.rollback event observed"
printf '%s\n' "old session alive: $team_session; mcp.server_exit/coordinator.session_missing absent"
