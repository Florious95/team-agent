from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


@dataclass(frozen=True)
class LineCountResult:
    path: str
    lines: int
    allowed: bool
    reason: str | None = None


ALLOWLIST_KEYS = {"approved_exceptions", "temporary_debt"}


def parse_line_count_allowlist_payload(payload: object, *, source: str = "line-count allowlist") -> dict[str, dict[str, Any]]:
    if not isinstance(payload, dict):
        raise ValueError(f"{source}: line-count allowlist must be a JSON object")
    unexpected = sorted(set(payload) - ALLOWLIST_KEYS)
    if unexpected:
        keys = ", ".join(unexpected)
        expected = ", ".join(sorted(ALLOWLIST_KEYS))
        raise ValueError(f"{source}: unexpected top-level key(s): {keys}; expected new schema keys: {expected}")
    approved = payload.get("approved_exceptions", {})
    temporary = payload.get("temporary_debt", {})
    if not isinstance(approved, dict):
        raise ValueError(f"{source}: approved_exceptions must be an object")
    if not isinstance(temporary, dict):
        raise ValueError(f"{source}: temporary_debt must be an object")
    for file_path, entry in approved.items():
        if not isinstance(entry, dict):
            raise ValueError(f"{source}: approved exception for {file_path} must be an object")
        max_lines = entry.get("max_lines")
        if not isinstance(max_lines, int) or max_lines < 1:
            raise ValueError(f"{source}: approved exception for {file_path} must declare positive integer max_lines")
    return {"approved_exceptions": approved, "temporary_debt": temporary}


def load_line_count_allowlist(path: Path) -> dict[str, dict[str, Any]]:
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return {"approved_exceptions": {}, "temporary_debt": {}}
    try:
        data = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{path}: invalid JSON in line-count allowlist: {exc}") from exc
    return parse_line_count_allowlist_payload(data, source=str(path))


def check_python_file_line_counts(
    root: Path,
    *,
    allowlist_path: Path,
    include_roots: Iterable[str] = ("src/team_agent", "tests"),
    max_lines: int = 500,
) -> list[LineCountResult]:
    allowlist = load_line_count_allowlist(allowlist_path)
    approved = allowlist["approved_exceptions"]
    results: list[LineCountResult] = []
    for relative_path in _iter_python_files(root, include_roots):
        full_path = root / relative_path
        line_count = _line_count(full_path)
        entry = approved.get(relative_path)
        if line_count <= max_lines:
            results.append(LineCountResult(relative_path, line_count, allowed=True))
            continue
        if entry:
            allowed_max = int(entry["max_lines"])
            results.append(
                LineCountResult(
                    relative_path,
                    line_count,
                    allowed=line_count <= allowed_max,
                    reason=str(entry.get("reason") or ""),
                )
            )
            continue
        results.append(LineCountResult(relative_path, line_count, allowed=False))
    return results


def line_count_failures(results: Iterable[LineCountResult]) -> list[LineCountResult]:
    return [result for result in results if not result.allowed]


def _iter_python_files(root: Path, include_roots: Iterable[str]) -> list[str]:
    paths: list[str] = []
    for include_root in include_roots:
        base = root / include_root
        if not base.exists():
            continue
        for path in base.rglob("*.py"):
            if "__pycache__" in path.parts:
                continue
            paths.append(path.relative_to(root).as_posix())
    return sorted(paths)


def _line_count(path: Path) -> int:
    return len(path.read_text(encoding="utf-8").splitlines())
