#!/usr/bin/env bash
set -u

socket_dir="/private/tmp/tmux-$(id -u)"
older_than_minutes=20
created_after_epoch=""
dry_run=0
verbose=0

usage() {
  cat <<'USAGE'
usage: cleanup-tmux-residue.sh [--older-than-minutes N] [--created-after-epoch EPOCH] [--socket-dir DIR] [--dry-run] [--verbose]

Safely cleans Team Agent test tmux residue under /private/tmp/tmux-$(id -u).
Only socket basenames matching ta-* or adv-* are eligible. The default socket is
never matched or touched.
USAGE
}

mtime_epoch() {
  local path="$1"
  if stat -f %m "$path" >/dev/null 2>&1; then
    stat -f %m "$path"
  else
    stat -c %Y "$path"
  fi
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --older-than-minutes)
      older_than_minutes="${2:?missing value for --older-than-minutes}"
      shift 2
      ;;
    --created-after-epoch)
      created_after_epoch="${2:?missing value for --created-after-epoch}"
      shift 2
      ;;
    --socket-dir)
      socket_dir="${2:?missing value for --socket-dir}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --verbose)
      verbose=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "cleanup-tmux-residue: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ ! "$older_than_minutes" =~ ^[0-9]+$ ]]; then
  echo "cleanup-tmux-residue: --older-than-minutes must be a non-negative integer" >&2
  exit 2
fi
if [[ -n "$created_after_epoch" && ! "$created_after_epoch" =~ ^[0-9]+$ ]]; then
  echo "cleanup-tmux-residue: --created-after-epoch must be a unix epoch integer" >&2
  exit 2
fi
if [[ ! -d "$socket_dir" ]]; then
  echo "cleanup-tmux-residue: socket_dir=$socket_dir missing candidates=0 killed=0 removed=0 skipped_recent=0 skipped_non_socket=0 errors=0"
  exit 0
fi

now_epoch="$(date +%s)"
older_than_seconds=$((older_than_minutes * 60))
candidates=0
killed=0
removed=0
skipped_recent=0
skipped_non_socket=0
errors=0

for socket in "$socket_dir"/ta-* "$socket_dir"/adv-*; do
  [[ -e "$socket" || -S "$socket" ]] || continue

  base="$(basename "$socket")"
  case "$base" in
    ta-*|adv-*) ;;
    *) continue ;;
  esac

  if [[ ! -S "$socket" ]]; then
    skipped_non_socket=$((skipped_non_socket + 1))
    [[ "$verbose" -eq 1 ]] && echo "cleanup-tmux-residue: skip non-socket $socket"
    continue
  fi

  mtime="$(mtime_epoch "$socket" 2>/dev/null || echo 0)"
  if [[ -n "$created_after_epoch" ]]; then
    if (( mtime < created_after_epoch )); then
      skipped_recent=$((skipped_recent + 1))
      continue
    fi
  elif (( now_epoch - mtime < older_than_seconds )); then
    skipped_recent=$((skipped_recent + 1))
    continue
  fi

  candidates=$((candidates + 1))
  if [[ "$dry_run" -eq 1 ]]; then
    [[ "$verbose" -eq 1 ]] && echo "cleanup-tmux-residue: dry-run $socket"
    continue
  fi

  if tmux -S "$socket" kill-server >/dev/null 2>&1; then
    killed=$((killed + 1))
  fi
  if [[ -e "$socket" || -S "$socket" ]]; then
    if rm -f "$socket"; then
      removed=$((removed + 1))
    else
      errors=$((errors + 1))
      echo "cleanup-tmux-residue: failed to remove $socket" >&2
    fi
  fi
done

mode="older_than_minutes=$older_than_minutes"
if [[ -n "$created_after_epoch" ]]; then
  mode="created_after_epoch=$created_after_epoch"
fi

echo "cleanup-tmux-residue: socket_dir=$socket_dir mode=$mode candidates=$candidates killed=$killed removed=$removed skipped_recent=$skipped_recent skipped_non_socket=$skipped_non_socket errors=$errors"

if [[ "$errors" -ne 0 ]]; then
  exit 1
fi
