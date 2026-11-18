#!/usr/bin/env bash
# Zero-residue oracle for a full test run.
#
#   scripts/test-residue.sh snapshot <run-dir> <out>   # before and after the suite
#   scripts/test-residue.sh diff <before> <after>      # fails on any new residue
#
# <run-dir> is the single directory the run owns (CARGO_TARGET_DIR, TMUX_TMPDIR,
# XDG_STATE_HOME and TMPDIR all point inside it). Only bounded locations are read:
# no recursive scan of /tmp.
set -euo pipefail

snapshot() { # <run-dir>
  local run=$1 uid
  uid=$(id -u)
  echo "## tmux sockets"
  for dir in "/tmp/tmux-$uid" "/private/tmp/tmux-$uid" "${TMUX_TMPDIR:-$run/tmux}/tmux-$uid"; do
    [ -d "$dir" ] && for s in "$dir"/*; do [ -S "$s" ] && echo "$s"; done
  done | LC_ALL=C sort -u
  echo "## live tmux servers"
  pgrep -fl 'tmux' 2>/dev/null | grep -v -- ' -C ' | awk '{$1=""; print}' | LC_ALL=C sort -u || true
  echo "## processes referencing the run dir"
  pgrep -fl -- "$run" 2>/dev/null | grep -v 'test-residue.sh' | awk '{$1=""; print}' | LC_ALL=C sort -u || true
  echo "## fixture temp roots"
  for g in /tmp/ta-* /private/tmp/ta-* /tmp/ddn* /private/tmp/ddn* "${TMPDIR:-/tmp}"/ta-*; do
    [ -e "$g" ] && echo "$g"
  done | LC_ALL=C sort -u
  echo "## state home"
  [ -d "${XDG_STATE_HOME:-$run/state}/team-agent" ] && ls -A "${XDG_STATE_HOME:-$run/state}/team-agent" || true
  echo "## checkout"
  [ -d "$run/src" ] && git -C "$run/src" status --porcelain --ignored=no || true
}

case "${1:-}" in
  snapshot) snapshot "$2" >"$3" ;;
  diff)
    new=$(LC_ALL=C comm -13 <(LC_ALL=C sort -u "$2") <(LC_ALL=C sort -u "$3") | grep -v '^## ' || true)
    if [ -n "$new" ]; then echo "RESIDUE after the run:"; echo "$new"; exit 1; fi
    echo "zero residue"
    ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
