#!/usr/bin/env bash
# purpose: t.p6.repro — 复现「投递通道清单」人读默认面不合用
# contract: 对 `team-agent leaders` 默认输出做四条机械断言,全满足 exit 0(绿);
#           任一不满足 exit 1(红=复现); 命令不存在/量具不对/命令失败 exit 2。
# boundary: 只读 CLI(`leaders`); 不 send、不碰生产席位、不读 .env、ps 不用。
#           断言打在默认人读面,不打 --json(机器面字段兼容可留)。
#
# 预算: 单次 <10s。高载不改变本装置(确定性字符串,无计时窗)。

set -u

GAUGE="${TEAM_AGENT_BIN:-/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent}"
EXPECT_MD5="${TEAM_AGENT_MD5:-2b7cf51937ea2d50897eeba75fae3b6b}"

say() { printf '%s\n' "$*"; }
die2() { say "UNJUDGEABLE: $*"; exit 2; }

if [[ ! -x "$GAUGE" ]]; then
  die2 "gauge missing: $GAUGE"
fi
GOT_MD5="$(md5 -q "$GAUGE" 2>/dev/null || true)"
if [[ "$GOT_MD5" != "$EXPECT_MD5" ]]; then
  die2 "gauge md5 mismatch got=${GOT_MD5:-empty} expect=$EXPECT_MD5 path=$GAUGE"
fi
MTIME="$(stat -f '%Sm' "$GAUGE")"
SIZE="$(stat -f '%z' "$GAUGE")"
VER="$("$GAUGE" --version 2>/dev/null || true)"
say "GAUGE path=$GAUGE md5=$GOT_MD5 mtime=$MTIME size=$SIZE version=$VER"

# 阳性对照:未知动词必须是 invalid choice,证明本量具的「命令不存在」检索有效。
# 取退出码不经管道。
set +e
"$GAUGE" channels --help >/tmp/ta-t107-channels.out 2>/tmp/ta-t107-channels.err
CH_RC=$?
set -e
if ! grep -q "invalid choice: 'channels'" /tmp/ta-t107-channels.err; then
  die2 "positive control failed: expected invalid choice for channels (rc=$CH_RC)"
fi
say "POSITIVE_CONTROL channels invalid-choice rc=$CH_RC"

set +e
"$GAUGE" leaders --help >/tmp/ta-t107-leaders-help.out 2>/tmp/ta-t107-leaders-help.err
HELP_RC=$?
set -e
HELP_ALL="$(cat /tmp/ta-t107-leaders-help.out /tmp/ta-t107-leaders-help.err)"
if grep -q "invalid choice: 'leaders'" <<<"$HELP_ALL"; then
  say "UNJUDGEABLE: command team-agent leaders does not exist on this gauge"
  exit 2
fi
if [[ "$HELP_RC" -ne 0 ]]; then
  die2 "leaders --help rc=$HELP_RC (not invalid-choice; cannot judge)"
fi
say "CMD=team-agent leaders (help rc=0)"

OUT="$(mktemp /tmp/ta-t107-leaders.XXXXXX)"
ERR="$(mktemp /tmp/ta-t107-leaders.XXXXXX)"
set +e
"$GAUGE" leaders >"$OUT" 2>"$ERR"
CMD_RC=$?
set -e
say "LEADERS_RC=$CMD_RC out_bytes=$(wc -c <"$OUT" | tr -d ' ') err_bytes=$(wc -c <"$ERR" | tr -d ' ')"
if [[ "$CMD_RC" -ne 0 ]]; then
  say "STDERR:"
  cat "$ERR"
  die2 "leaders exited $CMD_RC; listing surface not usable as a judge target"
fi
if [[ ! -s "$OUT" ]]; then
  die2 "leaders stdout empty"
fi

# 四条同时成立才绿。grep 写具体模式,不是永真/永假。
# A 节点所在工程:可复制 send 的 TO 以绝对路径开头
# B 该工程角色名:TO 含 ::<team>/<role>
# C 完整可复制:team-agent send '<abs>::<team>/<role>' '<msg>'
# D 默认面不含通道/hash-id 类字段(投递不走这些)
FAIL=0

if grep -E -q "team-agent send ['\"]/" "$OUT"; then
  say "PASS A: send TO starts with absolute workspace path"
else
  say "FAIL A: no copyable send TO starting with '/' (absolute workspace path)"
  FAIL=1
fi

if grep -E -q "team-agent send ['\"][^'\"]+::[^/'\" ]+/[^'\" ]+['\"]" "$OUT"; then
  say "PASS B: send TO contains ::<team>/<role>"
else
  say "FAIL B: no ::<team>/<role> inside a quoted send TO"
  FAIL=1
fi

if grep -E -q "team-agent send ['\"]/[^'\"]+::[^/'\" ]+/[^'\"]+['\"] ['\"]" "$OUT"; then
  say "PASS C: complete copyable send '<ws>::<team>/<role>' '<msg>'"
else
  say "FAIL C: no complete copyable line team-agent send '<ws>::<team>/<role>' '<msg>'"
  FAIL=1
fi

if grep -E -q 'workspace_hash|stable_qualified_name|channel_id|"channel":|pane_id' "$OUT"; then
  say "FAIL D: default surface still prints channel/hash-id class fields:"
  grep -E -o 'workspace_hash|stable_qualified_name|channel_id|"channel":|pane_id' "$OUT" | sort -u | sed 's/^/  /'
  FAIL=1
else
  say "PASS D: default surface has no workspace_hash/stable_qualified_name/channel_id/pane_id"
fi

say "---- stdout (truncated to 8k for the log; full file $OUT) ----"
head -c 8192 "$OUT"
say ""
say "---- end stdout ----"

if [[ "$FAIL" -eq 0 ]]; then
  say "GREEN: leaders default surface has workspace path + team/role + copyable send, and no channel/hash-id noise"
  exit 0
fi
say "RED: leaders default surface unusable (failed assertions above)"
exit 1
