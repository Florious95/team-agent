from __future__ import annotations

from typing import Any

TASK_STATUSES = {
    "pending",
    "ready",
    "running",
    "blocked",
    "needs_retry",
    "done",
    "failed",
    "cancelled",
}

TERMINAL_TASK_STATUSES = {"done", "failed", "cancelled"}


def find_dependency_cycle(tasks: list[dict[str, Any]]) -> list[str]:
    graph = {t.get("id"): list(t.get("deps", [])) for t in tasks if t.get("id")}
    visiting: set[str] = set()
    visited: set[str] = set()
    stack: list[str] = []

    def visit(node: str) -> list[str] | None:
        if node in visited:
            return None
        if node in visiting:
            if node in stack:
                return stack[stack.index(node) :] + [node]
            return [node, node]
        visiting.add(node)
        stack.append(node)
        for dep in graph.get(node, []):
            if dep in graph:
                cycle = visit(dep)
                if cycle:
                    return cycle
        stack.pop()
        visiting.remove(node)
        visited.add(node)
        return None

    for node in graph:
        cycle = visit(node)
        if cycle:
            return cycle
    return []


def ready_tasks(tasks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_id = {t["id"]: t for t in tasks}
    ready: list[dict[str, Any]] = []
    for task in tasks:
        if task.get("status", "pending") not in {"pending", "ready", "needs_retry"}:
            continue
        deps_done = all(by_id.get(dep, {}).get("status") == "done" for dep in task.get("deps", []))
        if deps_done:
            ready.append(task)
    return ready


def update_task_status(
    tasks: list[dict[str, Any]],
    task_id: str,
    status: str,
    summary: str | None = None,
    artifact_refs: list[dict[str, Any]] | None = None,
) -> None:
    if status not in TASK_STATUSES:
        raise ValueError(f"Unknown task status: {status}")
    for task in tasks:
        if task.get("id") == task_id:
            task["status"] = status
            if summary is not None:
                task["last_result_summary"] = summary
            if artifact_refs is not None:
                task["artifact_refs"] = artifact_refs
            return
    raise KeyError(f"Unknown task id: {task_id}")
