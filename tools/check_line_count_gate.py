#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fnmatch
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SRC_ROOT = REPO_ROOT / "src"
if str(SRC_ROOT) not in sys.path:
    sys.path.insert(0, str(SRC_ROOT))

from team_agent.quality_gates import load_line_count_allowlist


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


def _load_allowlist(path: Path | None) -> tuple[dict[str, object], str | None]:
    if path is None or not path.exists():
        return {"approved_exceptions": {}, "temporary_debt": {}}, None
    try:
        return load_line_count_allowlist(path), None
    except ValueError as exc:
        return {}, str(exc)


def _approved_ceiling(entry: object, path: str) -> tuple[int | None, str | None]:
    if not isinstance(entry, dict):
        return None, f"approved exception for {path} must be an object"
    max_lines = entry.get("max_lines")
    if not isinstance(max_lines, int) or max_lines < 1:
        return None, f"approved exception for {path} must declare positive integer max_lines"
    return max_lines, None


def check_gate(
    *,
    root: Path,
    glob_pattern: str,
    max_lines: int,
    allowlist: Path | None,
    require_empty_temporary_debt: bool,
    hard: bool,
) -> int:
    if not root.exists() or not root.is_dir():
        print(f"line-count gate: root is not a directory: {root}", file=sys.stderr)
        return 1

    allowlist_payload, allowlist_error = _load_allowlist(allowlist)
    approved = allowlist_payload.get("approved_exceptions", {})
    temporary_debt = allowlist_payload.get("temporary_debt", {})
    approved_count = 0
    temporary_debt_count = len(temporary_debt) if isinstance(temporary_debt, dict) else 0
    config_errors: list[str] = []
    over_limit: list[tuple[Path, int, int]] = []
    checked = 0
    for path in _iter_matching_files(root, glob_pattern):
        checked += 1
        count = _line_count(path)
        relative = path.relative_to(root).as_posix()
        allowlist_keys = (path.as_posix(), relative)
        limit = max_lines
        approved_key = None
        if isinstance(approved, dict):
            approved_key = next(
                (
                    key
                    for key in approved
                    if key in allowlist_keys or key.endswith(f"/{relative}")
                ),
                None,
            )
        if approved_key:
            approved_count += 1
            approved_limit, error = _approved_ceiling(approved[approved_key], approved_key)
            if error:
                config_errors.append(error)
            elif approved_limit is not None:
                limit = approved_limit
        if count > limit:
            over_limit.append((path, count, limit))

    for path, count, limit in over_limit:
        print(f"{path}: {count} lines > {limit}")

    if allowlist_error:
        print(f"line-count gate: {allowlist_error}: {allowlist}", file=sys.stderr)
    for error in config_errors:
        print(f"line-count gate: {error}: {allowlist}", file=sys.stderr)
    temporary_debt_failed = require_empty_temporary_debt and temporary_debt_count > 0
    if temporary_debt_failed:
        print(f"line-count gate: temporary_debt must be empty for completion: {allowlist}", file=sys.stderr)
    print(
        "passed: "
        f"{checked - len(over_limit)} files; "
        f"approved exceptions: {approved_count} files (with ceilings); "
        f"over-limit: {len(over_limit)} files; "
        f"temporary_debt entries: {temporary_debt_count} (must be 0 for completion)"
    )

    if allowlist_error or config_errors or temporary_debt_failed:
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
    parser.add_argument("--require-empty-temporary-debt", action="store_true")
    parser.add_argument("--hard", action="store_true")
    args = parser.parse_args(argv)

    return check_gate(
        root=args.root,
        glob_pattern=args.glob_pattern,
        max_lines=args.max_lines,
        allowlist=args.allowlist,
        require_empty_temporary_debt=args.require_empty_temporary_debt,
        hard=args.hard,
    )


if __name__ == "__main__":
    raise SystemExit(main())
