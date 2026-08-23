#!/bin/sh
# purpose: 席位交付物的四态判据——验痕迹（产物存在且自陈了明确结论），⛔ 不重做事实
# contract: 退出码即判据结果 0=pass / 1=fail / 2=不可判(产物缺失·空·无 verdict 行·多个矛盾结论)
# boundary: 只读文件、只认独立成行的 verdict 声明；⛔ 不跑测试、⛔ 不读代码、⛔ 不联网
#
# 为什么只验痕迹：判据验证痕迹、不重做事实（铁律④）。
# 席位跑没跑测试、破坏齿有没有牙，由判者去核；本脚本只回答
# 「这一格有没有交出一份自陈了明确结论的产物」——它是流转的闸，不是质量的闸。
#
# 🔴 必须是 POSIX sh：ScriptRef 的编译期门是 `sh -n`，
# mapfile / <(...) 这类 bash-only 语法会当场被拒（2026-08-23 实撞）。

set -u
RC_PASS=0; RC_FAIL=1; RC_UNJUDGEABLE=2

log() { printf '[check_verdict] %s\n' "$*" >&2; }

if [ $# -lt 1 ]; then
  log "用法: check_verdict.sh <产物路径> [--expect pass|fail]"
  exit $RC_UNJUDGEABLE
fi
F="$1"; shift
EXPECT="pass"
if [ "${1:-}" = "--expect" ]; then EXPECT="${2:-pass}"; shift 2; fi

[ -f "$F" ] || { log "产物不存在: $F ⇒ 不可判"; exit $RC_UNJUDGEABLE; }
[ -s "$F" ] || { log "产物为空: $F ⇒ 不可判"; exit $RC_UNJUDGEABLE; }

# 只认独立成行的 verdict 声明。⛔ 不做宽松子串匹配——
# 正文里出现「the verdict: pass is inline」这类行内提及不算数（子串碰撞今天已实撞过一次）。
HITS=$(grep -E '^[[:space:]]*verdict:[[:space:]]*(pass|fail|unjudgeable|not_applicable)[[:space:]]*$' "$F" \
       | sed -E 's/^[[:space:]]*verdict:[[:space:]]*//; s/[[:space:]]*$//')

if [ -z "$HITS" ]; then
  log "未找到独立成行的 verdict: 声明 ⇒ 不可判（⛔ 不当作 fail）"
  exit $RC_UNJUDGEABLE
fi

# 多个结论互相矛盾 ⇒ 不可判。⛔ 不许取最后一个了事——那是替席位做决定。
UNIQ=$(printf '%s\n' "$HITS" | sort -u | wc -l | tr -d ' ')
if [ "$UNIQ" -ne 1 ]; then
  log "产物含互相矛盾的 verdict: $(printf '%s' "$HITS" | tr '\n' ' ') ⇒ 不可判"
  exit $RC_UNJUDGEABLE
fi

V=$(printf '%s\n' "$HITS" | head -n1)
log "verdict=$V (expect=$EXPECT)"
case "$V" in
  unjudgeable|not_applicable) exit $RC_UNJUDGEABLE;;
  "$EXPECT")                  exit $RC_PASS;;
  *)                          exit $RC_FAIL;;
esac
