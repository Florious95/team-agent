from __future__ import annotations

import json
import os
import re
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Any

_PLAN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")


class InvalidPlanIdError(ValueError):
    pass


def sanitize_plan_id(plan_id: Any) -> str:
    text = "" if plan_id is None else str(plan_id).strip()
    if not _PLAN_ID_RE.match(text):
        raise InvalidPlanIdError(
            f"invalid plan id {text!r}: must match [A-Za-z0-9][A-Za-z0-9_.-]{{0,63}} "
            "(no slashes, no spaces, no path-traversal characters)"
        )
    return text


def _orchestrator_runtime_dir(workspace: Path) -> Path:
    return Path(workspace) / ".team" / "runtime" / "orchestrator"


def _orchestrator_artifact_dir(workspace: Path) -> Path:
    return Path(workspace) / ".team" / "artifacts" / "orchestrator"


def state_path(workspace: Path, plan_id: str) -> Path:
    safe = sanitize_plan_id(plan_id)
    return _orchestrator_runtime_dir(workspace) / f"plan-{safe}.state.json"


def artifact_path(workspace: Path, plan_id: str, ts: str) -> Path:
    safe = sanitize_plan_id(plan_id)
    return _orchestrator_artifact_dir(workspace) / f"halt-{safe}-{ts}.md"


@contextmanager
def _plan_state_lock(workspace: Path, plan_id: str, timeout: float = 5.0):
    import fcntl
    import time
    safe = sanitize_plan_id(plan_id)
    lock_dir = _orchestrator_runtime_dir(workspace)
    lock_dir.mkdir(parents=True, exist_ok=True)
    lock_path = lock_dir / f"plan-{safe}.lock"
    deadline = time.monotonic() + timeout
    with lock_path.open("w", encoding="utf-8") as lock_file:
        while True:
            try:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise RuntimeError(
                        f"orchestrator plan {safe} state file is locked by another process; retry"
                    )
                time.sleep(0.05)
        try:
            yield
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def load_plan_state(workspace: Path, plan_id: str) -> dict[str, Any] | None:
    path = state_path(workspace, plan_id)
    if not path.exists():
        return None
    with _plan_state_lock(workspace, plan_id):
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            return None


def save_plan_state(workspace: Path, state: dict[str, Any]) -> Path:
    plan_id = sanitize_plan_id(state.get("plan_id"))
    path = state_path(workspace, plan_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(state, indent=2, ensure_ascii=False, sort_keys=True)
    with _plan_state_lock(workspace, plan_id):
        fd, tmp_name = tempfile.mkstemp(
            prefix=f".plan-{plan_id}.", suffix=".tmp", dir=str(path.parent)
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as tmp_file:
                tmp_file.write(payload)
                tmp_file.flush()
                os.fsync(tmp_file.fileno())
            os.replace(tmp_name, path)
        except Exception:
            try:
                os.unlink(tmp_name)
            except FileNotFoundError:
                pass
            raise
    return path


def list_plan_states(workspace: Path) -> list[dict[str, Any]]:
    directory = _orchestrator_runtime_dir(workspace)
    if not directory.exists():
        return []
    out: list[dict[str, Any]] = []
    for path in sorted(directory.glob("plan-*.state.json")):
        try:
            out.append(json.loads(path.read_text(encoding="utf-8")))
        except json.JSONDecodeError:
            continue
    return out


__all__ = [
    "InvalidPlanIdError",
    "artifact_path",
    "list_plan_states",
    "load_plan_state",
    "sanitize_plan_id",
    "save_plan_state",
    "state_path",
]
