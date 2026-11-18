#!/usr/bin/env bash
# Exact test-inventory parity between two git refs.
#
#   scripts/test-inventory.sh list <ref> <out.tsv>
#       Every test of the root workspace and the independent contract workspace:
#       "<workspace>\t<binary source>\t<test name>\t<ignored 0|1>", sorted.
#   scripts/test-inventory.sh diff <before.tsv> <after.tsv> <retired.txt> <added.txt>
#       Fails unless REMOVED == retired.txt and ADDED == added.txt exactly
#       (one "<binary source>\t<test name>" per line), and no test flips to ignored
#       unless it is listed in added.txt.
set -euo pipefail

list_workspace() { # <label> <dir> [cargo args...]
  local label=$1 dir=$2 tmp
  tmp=$(mktemp -d)
  (cd "$dir" && cargo test --locked "${@:3}" -- --list --format terse) >"$tmp/all" 2>&1
  (cd "$dir" && cargo test --locked "${@:3}" -- --list --format terse --ignored) >"$tmp/ignored" 2>&1
  # Binary identity is its source path from cargo's "Running <src> (<artifact>)" line.
  awk -v label="$label" '
    FNR == 1 { bin = "" }
    /^ *Running / { bin = $0; sub(/^ *Running /, "", bin); sub(/ \(.*$/, "", bin); next }
    /: test$/ { t = $0; sub(/: test$/, "", t)
      if (NR == FNR) ign[bin "\t" t] = 1
      else print label "\t" bin "\t" t "\t" (((bin "\t" t) in ign) ? 1 : 0) }
  ' "$tmp/ignored" "$tmp/all"
  rm -rf "$tmp"
}

case "${1:-}" in
  list)
    ref=$2 out=$3 work=$(mktemp -d)
    trap 'git worktree remove --force "$work/src" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
    git worktree add --detach "$work/src" "$ref" >/dev/null
    export CARGO_TARGET_DIR="${INVENTORY_TARGET_DIR:-$work/target}"
    { list_workspace root "$work/src" --workspace
      CARGO_TARGET_DIR="$CARGO_TARGET_DIR-contract" list_workspace contract "$work/src/crates/team-agent-contract"
    } | LC_ALL=C sort -u >"$out"
    echo "$(wc -l <"$out" | tr -d ' ') tests at $(git rev-parse --short "$ref") -> $out"
    ;;
  diff)
    before=$2 after=$3 retired=$4 added=$5 status=0
    key() { cut -f2,3 "$1" | LC_ALL=C sort -u; }
    removed=$(LC_ALL=C comm -23 <(key "$before") <(key "$after"))
    new=$(LC_ALL=C comm -13 <(key "$before") <(key "$after"))
    if ! diff -u <(LC_ALL=C sort -u "$retired") <(printf '%s\n' "$removed" | sed '/^$/d'); then
      echo "REMOVED set differs from the declared retire list" >&2; status=1; fi
    if ! diff -u <(LC_ALL=C sort -u "$added") <(printf '%s\n' "$new" | sed '/^$/d'); then
      echo "ADDED set differs from the declared new-test list" >&2; status=1; fi
    flipped=$(LC_ALL=C join -t $'\t' <(awk -F'\t' '$4==0{print $2"|"$3}' "$before" | LC_ALL=C sort -u) \
      <(awk -F'\t' '$4==1{print $2"|"$3}' "$after" | LC_ALL=C sort -u) || true)
    if [ -n "$flipped" ]; then echo "tests newly ignored:" >&2; echo "$flipped" >&2; status=1; fi
    echo "before=$(wc -l <"$before" | tr -d ' ') after=$(wc -l <"$after" | tr -d ' ') removed=$(printf '%s' "$removed" | grep -c . || true) added=$(printf '%s' "$new" | grep -c . || true)"
    exit $status
    ;;
  *) sed -n '2,12p' "$0"; exit 2 ;;
esac
