#!/usr/bin/env bash
# Zero-residue oracle for a full test run.
#
#   scripts/test-residue.sh snapshot <run-dir> <out> [checkout]   # before and after the suite
#   scripts/test-residue.sh diff <before> <after>      # fails on any new residue
#
# <run-dir> is the single directory the run owns; the runner points TMUX_TMPDIR,
# XDG_STATE_HOME and TMPDIR inside it (macOS fixtures that hard-code /private/tmp are
# covered by the bounded globs). [checkout] defaults to <run-dir>/src. Only bounded
# locations are read: no recursive scan of /tmp.
set -euo pipefail

snapshot() { # <run-dir> [checkout]
  # Every section must complete: no `[ test ] && cmd` as a loop's last command, which
  # under `set -e -o pipefail` aborts the snapshot and makes before/after vacuously equal.
  local run=$1 checkout=${2:-} uid dir s g d e
  [ -n "$checkout" ] || checkout=$run/src
  uid=$(id -u)
  echo "## tmux sockets"
  for dir in "/tmp/tmux-$uid" "/private/tmp/tmux-$uid" "${TMUX_TMPDIR:-$run/tmux}/tmux-$uid"; do
    if [ -d "$dir" ]; then for s in "$dir"/*; do if [ -S "$s" ]; then echo "$s"; fi; done; fi
  done | LC_ALL=C sort -u
  echo "## live tmux servers"
  # Full argv on Linux and macOS (pgrep -fl prints only the name on Linux).
  { ps -axo args= 2>/dev/null || true; } | { grep -E '(^|/)tmux( |$)' || true; } | { grep -v -- ' -C ' || true; } | cut -c1-300 | LC_ALL=C sort -u
  echo "## processes referencing the run dir"
  { ps -axo args= 2>/dev/null || true; } | { grep -F -- "$run" || true; } \
    | { grep -v -e 'test-residue.sh' -e 'grep -F' || true; } | cut -c1-300 | LC_ALL=C sort -u
  echo "## fixture temp roots"
  for g in /tmp/ta-* /private/tmp/ta-* /tmp/ddn* /private/tmp/ddn* "${TMPDIR:-/tmp}"/ta-*; do
    if [ -e "$g" ]; then echo "$g"; fi
  done | LC_ALL=C sort -u
  echo "## state home"
  for d in "${XDG_STATE_HOME:-$run/state}/team-agent" "${XDG_STATE_HOME:-$run/state}"/team-agent/*; do
    if [ -d "$d" ]; then for e in "$d"/* "$d"/.[!.]*; do if [ -e "$e" ]; then echo "$e"; fi; done; fi
  done | LC_ALL=C sort -u
  echo "## checkout"
  if [ -d "$checkout" ]; then git -C "$checkout" status --porcelain --ignored=no; fi
  echo "## end"
}

case "${1:-}" in
  snapshot) snapshot "$2" "${4:-}" >"$3" ;;
  diff)
    for f in "$2" "$3"; do grep -qx '## end' "$f" || { echo "incomplete snapshot: $f" >&2; exit 2; }; done
    new=$(LC_ALL=C comm -13 <(LC_ALL=C sort -u "$2") <(LC_ALL=C sort -u "$3") | grep -v '^## ' || true)
    if [ -n "$new" ]; then echo "RESIDUE after the run:"; echo "$new"; exit 1; fi
    echo "zero residue"
    ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
