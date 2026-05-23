#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fnmatch
import json
import sys
from pathlib import Path


def _line_count(path: Path) -> int:
    with path.open("rb") as handle:
        return sum(1 for _ in handle)


def _iter_matching_files(root: Path, pattern: str) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file()
        and (
            fnmatch.fnmatch(path.name, pattern)
            or fnmatch.fnmatch(path.relative_to(root).as_posix(), pattern)
        )
    )


def _json_payload_has_entries(payload: object) -> bool:
    if payload is None:
        return False
    if isinstance(payload, (list, tuple, set, dict)):
        return bool(payload)
    return True


def _allowlist_has_entries(path: Path | None) -> tuple[bool, str | None]:
    if path is None or not path.exists():
        return False, None
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return False, None
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as exc:
        return True, f"invalid JSON in line-count allowlist: {exc}"
    if isinstance(payload, dict) and "temporary_allowlist" in payload:
        return _json_payload_has_entries(payload.get("temporary_allowlist")), None
    return _json_payload_has_entries(payload), None


def check_gate(
    *,
    root: Path,
    glob_pattern: str,
    max_lines: int,
    allowlist: Path | None,
    require_empty_allowlist: bool,
    hard: bool,
) -> int:
    if not root.exists() or not root.is_dir():
        print(f"line-count gate: root is not a directory: {root}", file=sys.stderr)
        return 1

    over_limit: list[tuple[Path, int]] = []
    for path in _iter_matching_files(root, glob_pattern):
        count = _line_count(path)
        if count > max_lines:
            over_limit.append((path, count))

    for path, count in over_limit:
        print(f"{path}: {count} lines > {max_lines}")

    allowlist_has_entries, allowlist_error = _allowlist_has_entries(allowlist)
    allowlist_failed = require_empty_allowlist and allowlist_has_entries
    if allowlist_failed:
        if allowlist_error:
            print(f"line-count gate: {allowlist_error}: {allowlist}", file=sys.stderr)
        else:
            print(f"line-count gate: allowlist must be empty for completion: {allowlist}", file=sys.stderr)

    if allowlist_failed:
        return 1
    if hard and over_limit:
        return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Fail completion when production files exceed a line-count limit.")
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--glob", required=True, dest="glob_pattern")
    parser.add_argument("--max-lines", required=True, type=int)
    parser.add_argument("--allowlist", type=Path)
    parser.add_argument("--require-empty-allowlist", action="store_true")
    parser.add_argument("--hard", action="store_true")
    args = parser.parse_args(argv)

    return check_gate(
        root=args.root,
        glob_pattern=args.glob_pattern,
        max_lines=args.max_lines,
        allowlist=args.allowlist,
        require_empty_allowlist=args.require_empty_allowlist,
        hard=args.hard,
    )


if __name__ == "__main__":
    raise SystemExit(main())
