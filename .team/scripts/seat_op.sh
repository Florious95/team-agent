#!/usr/bin/env bash
# purpose: 席位生命周期动作的判据封装——供账本 mechanical acceptance 直接当 argv 调用
# contract: 退出码即判据结果 0=目标态达成 / 1=确证未达成 / 2=不可判(环境不可达·信号矛盾·参数不足)
# boundary: 只做 add/start/stop/probe 四个动作与其验证；⛔ 不杀进程、⛔ 不清 socket、⛔ 不碰 runtime 目录
#
# 为什么必须封装（2026-08-23 实测）：
#   team-agent status 的退出码在两种情况下都说谎——
#     ① `status <AGENT>` 的 AGENT 位置参数被忽略，传不存在的名字照样输出全部席位、rc=0
#     ② `status --team <不存在>` 返回 {"ok":false,...} 但 rc=0
#   ⇒ 裸 grep 会把「team 不可达」误判成「席位没起来」，即把不可判折成失败。
#   本脚本一律解析 JSON 的 ok 字段 + agents 精确键，⛔ 不信 status 的退出码。

set -uo pipefail

RC_OK=0; RC_FAIL=1; RC_UNJUDGEABLE=2

usage() {
  cat >&2 <<'EOF'
用法: seat_op.sh <op> <agent> [选项]
  op:  ensure-up | ensure-down | restart | probe
  --role-file F   ensure-up 时若席位不存在，用它 add-agent（席位已存在则不需要）
  --workspace W   默认 $PWD
  --team T        必填（或设 TEAM_AGENT_TEAM）
  --timeout S     单次验证轮询总预算秒数，默认 90
  --force         传给 add-agent/start-agent
退出码: 0=目标态达成  1=确证未达成  2=不可判
EOF
}

log() { printf '[seat_op] %s\n' "$*" >&2; }

# macOS 无 timeout(1)：用 perl alarm 兜底
run_bounded() {
  local secs="$1"; shift
  perl -e 'alarm shift; exec @ARGV' "$secs" "$@"
}

OP=""; AGENT=""; ROLE_FILE=""; WS="$PWD"; TEAM="${TEAM_AGENT_TEAM:-}"; BUDGET=90; FORCE=""
[ $# -ge 2 ] || { usage; exit $RC_UNJUDGEABLE; }
OP="$1"; AGENT="$2"; shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --role-file) ROLE_FILE="${2:-}"; shift 2;;
    --workspace) WS="${2:-}"; shift 2;;
    --team)      TEAM="${2:-}"; shift 2;;
    --timeout)   BUDGET="${2:-}"; shift 2;;
    --force)     FORCE="--force"; shift;;
    *) log "未知参数: $1"; usage; exit $RC_UNJUDGEABLE;;
  esac
done

[ -n "$TEAM" ]  || { log "缺 --team ⇒ 不可判"; exit $RC_UNJUDGEABLE; }
[ -n "$AGENT" ] || { log "缺 agent ⇒ 不可判"; exit $RC_UNJUDGEABLE; }
case "$OP" in ensure-up|ensure-down|restart|probe) ;; *) log "未知 op: $OP"; exit $RC_UNJUDGEABLE;; esac

# probe_state:
#   stdout = 该席位状态词(running/其它) 或 __absent__
#   退出码 0=查询可信 / 2=查询本身不可信(团队不可达·JSON 坏·超时·零席位)
# 「零席位」也判不可信：team 存在但一个席位都没有，多半是状态目录异常，
# 此时说「目标席位不存在」没有分辨力——阳性对照失效（§5 信号说谎）。
probe_state() {
  local out rc
  out="$(run_bounded 30 team-agent status --workspace "$WS" --team "$TEAM" --json 2>/dev/null)"
  rc=$?
  if [ $rc -ne 0 ] || [ -z "$out" ]; then
    log "status 调用失败或超时(rc=$rc) ⇒ 不可判"
    return $RC_UNJUDGEABLE
  fi
  printf '%s' "$out" | AGENT="$AGENT" python3 -c '
import json, os, sys
try:
    d = json.load(sys.stdin)
except Exception as e:
    print("JSONBAD:%s" % e, file=sys.stderr); sys.exit(2)
if d.get("ok") is False:
    print("NOTOK:%s" % d.get("error", "")[:200], file=sys.stderr); sys.exit(2)
ag = d.get("agents")
if not isinstance(ag, dict) or not ag:
    print("NOAGENTS", file=sys.stderr); sys.exit(2)
me = ag.get(os.environ["AGENT"])
print(me.get("status", "unknown") if me is not None else "__absent__")
sys.exit(0)
'
}

# wait_for <期望语义: up|down> —— 轮询到目标态或耗尽预算
# 轮询期间任何一次 probe 不可判都直接向上传 2，⛔ 不当成「还没起来」继续等
wait_for() {
  local want="$1" waited=0 step=3 st prc
  while :; do
    st="$(probe_state)"; prc=$?
    [ $prc -eq $RC_UNJUDGEABLE ] && return $RC_UNJUDGEABLE
    case "$want" in
      up)   [ "$st" = "running" ] && return $RC_OK;;
      down) { [ "$st" = "__absent__" ] || [ "$st" != "running" ]; } && return $RC_OK;;
    esac
    [ "$waited" -ge "$BUDGET" ] && { log "等待 $want 超预算 ${BUDGET}s，末次状态=$st"; return $RC_FAIL; }
    sleep $step; waited=$((waited + step))
  done
}

# act <argv...> —— 动作命令带一次重试（仅对非破坏性动作调用方开启）
# ⚠️ 动作命令的退出码同样不可尽信 ⇒ 成败一律由随后的 wait_for 裁定，本函数只记录
act() {
  local rc
  run_bounded 120 "$@" >/dev/null 2>&1; rc=$?
  [ $rc -ne 0 ] && log "动作返回 rc=$rc（不作判据，以状态验证为准）: $1 $2 $3"
  return 0
}

st="$(probe_state)"; prc=$?
[ $prc -eq $RC_UNJUDGEABLE ] && exit $RC_UNJUDGEABLE

case "$OP" in
  probe)
    log "$AGENT = $st"
    [ "$st" = "running" ] && exit $RC_OK || exit $RC_FAIL
    ;;

  ensure-up)
    [ "$st" = "running" ] && { log "$AGENT 已 running（幂等）"; exit $RC_OK; }
    if [ "$st" = "__absent__" ]; then
      [ -n "$ROLE_FILE" ] || { log "$AGENT 不存在且未给 --role-file ⇒ 不可判"; exit $RC_UNJUDGEABLE; }
      [ -f "$ROLE_FILE" ] || { log "角色文件不存在: $ROLE_FILE ⇒ 不可判"; exit $RC_UNJUDGEABLE; }
      act team-agent add-agent "$AGENT" --role-file "$ROLE_FILE" --workspace "$WS" --team "$TEAM" $FORCE
    else
      act team-agent start-agent "$AGENT" --workspace "$WS" --team "$TEAM" $FORCE
    fi
    wait_for up; exit $?
    ;;

  ensure-down)
    [ "$st" = "__absent__" ] && { log "$AGENT 不存在（幂等）"; exit $RC_OK; }
    [ "$st" != "running" ]  && { log "$AGENT 已非 running: $st（幂等）"; exit $RC_OK; }
    # 破坏性动作只发一次，⛔ 不重试
    run_bounded 120 team-agent stop-agent "$AGENT" --workspace "$WS" --team "$TEAM" >/dev/null 2>&1
    wait_for down; exit $?
    ;;

  restart)
    if [ "$st" = "__absent__" ]; then log "$AGENT 不存在，无可重启 ⇒ 不可判"; exit $RC_UNJUDGEABLE; fi
    run_bounded 120 team-agent stop-agent "$AGENT" --workspace "$WS" --team "$TEAM" >/dev/null 2>&1
    wait_for down || { log "重启的停止相未达成"; exit $?; }
    act team-agent start-agent "$AGENT" --workspace "$WS" --team "$TEAM" $FORCE
    wait_for up; exit $?
    ;;
esac
