from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.orchestrator.plan import evaluate_condition, load_plan, stage_matches_result
from team_agent.orchestrator.state import (
    artifact_path,
    list_plan_states,
    load_plan_state,
    save_plan_state,
    state_path,
)


def start_plan(workspace: Path, plan_path: Path, *, start: bool = True) -> dict[str, Any]:
    workspace = Path(workspace)
    plan = load_plan(plan_path)
    plan_id = str(plan.get("id") or "").strip()
    if not plan_id:
        return {"ok": False, "error": "plan missing 'id'"}
    stages = list(plan.get("stages") or [])
    if not stages:
        return {"ok": False, "error": "plan has no stages", "plan_id": plan_id}
    event_log = EventLog(workspace)
    existing = load_plan_state(workspace, plan_id)
    if existing and existing.get("status") in {"running", "halted", "completed"}:
        return {
            "ok": True,
            "status": existing.get("status"),
            "plan_id": plan_id,
            "current_stage": existing.get("current_stage"),
            "already_started": True,
            "state_path": str(state_path(workspace, plan_id)),
        }
    state = {
        "plan_id": plan_id,
        "plan_path": str(plan_path),
        "current_stage": 1,
        "completed_stages": [],
        "status": "running",
        "halt_reason": None,
        "halt_artifact": None,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "stages": stages,
    }
    save_plan_state(workspace, state)
    event_log.write("orchestrator.plan_started", plan_id=plan_id, stage_count=len(stages))
    if start:
        _dispatch_stage(workspace, state, stages[0], event_log)
    return {
        "ok": True,
        "status": "running",
        "plan_id": plan_id,
        "current_stage": state["current_stage"],
        "state_path": str(state_path(workspace, plan_id)),
    }


def handle_report_result(workspace: Path, envelope: dict[str, Any]) -> dict[str, Any]:
    workspace = Path(workspace)
    event_log = EventLog(workspace)
    matched = None
    for state in list_plan_states(workspace):
        if state.get("status") != "running":
            continue
        idx = int(state.get("current_stage") or 1) - 1
        stages = state.get("stages") or []
        if idx < 0 or idx >= len(stages):
            continue
        stage = stages[idx]
        if not stage_matches_result(stage, envelope):
            continue
        matched = state
        break
    if matched is None:
        return {"ok": True, "status": "no_match", "matched": False}
    idx = int(matched["current_stage"]) - 1
    stages = matched["stages"]
    stage = stages[idx]
    halt_expr = stage.get("halt_on")
    advance_expr = stage.get("advance_on")
    if halt_expr and evaluate_condition(str(halt_expr), envelope):
        return _halt_plan(workspace, matched, stage, envelope, str(halt_expr), event_log)
    if advance_expr and evaluate_condition(str(advance_expr), envelope):
        matched["completed_stages"].append(matched["current_stage"])
        matched["current_stage"] += 1
        if matched["current_stage"] > len(stages):
            matched["status"] = "completed"
            matched["completed_at"] = datetime.now(timezone.utc).isoformat()
            save_plan_state(workspace, matched)
            event_log.write("orchestrator.plan_completed", plan_id=matched["plan_id"])
            return {
                "ok": True,
                "status": "completed",
                "plan_id": matched["plan_id"],
                "current_stage": matched["current_stage"],
            }
        save_plan_state(workspace, matched)
        next_stage = stages[matched["current_stage"] - 1]
        _dispatch_stage(workspace, matched, next_stage, event_log)
        return {
            "ok": True,
            "status": "running",
            "plan_id": matched["plan_id"],
            "current_stage": matched["current_stage"],
        }
    return {
        "ok": True,
        "status": "waiting",
        "plan_id": matched["plan_id"],
        "current_stage": matched["current_stage"],
        "matched": True,
    }


def resume_plans(workspace: Path) -> dict[str, Any]:
    workspace = Path(workspace)
    return {"ok": True, "plans": list_plan_states(workspace)}


def halt_plan(workspace: Path, plan_id: str, *, reason: str = "user_requested") -> dict[str, Any]:
    workspace = Path(workspace)
    state = load_plan_state(workspace, plan_id)
    if state is None:
        return {"ok": False, "error": "plan not found", "plan_id": plan_id}
    if state.get("status") != "running":
        return {
            "ok": True,
            "plan_id": plan_id,
            "status": state.get("status"),
            "halt_reason": state.get("halt_reason"),
            "halt_artifact": state.get("halt_artifact"),
            "already_terminal": True,
        }
    event_log = EventLog(workspace)
    idx = int(state.get("current_stage") or 1) - 1
    stages = state.get("stages") or []
    stage = stages[idx] if 0 <= idx < len(stages) else {"id": "unknown"}
    return _halt_plan(workspace, state, stage, {"reason": reason}, reason, event_log)


def plan_status(workspace: Path, plan_id: str | None = None) -> dict[str, Any]:
    workspace = Path(workspace)
    plans = list_plan_states(workspace)
    if plan_id is not None:
        match = next((state for state in plans if state.get("plan_id") == plan_id), None)
        if match is None:
            return {"ok": False, "error": "plan not found", "plan_id": plan_id}
        return {"ok": True, "plan": match}
    return {"ok": True, "plans": plans}


def _dispatch_stage(workspace: Path, state: dict[str, Any], stage: dict[str, Any], event_log: EventLog) -> None:
    from team_agent import runtime
    dispatch = stage.get("dispatch") or {}
    to = dispatch.get("to")
    content = dispatch.get("content")
    if not to or content is None:
        event_log.write(
            "orchestrator.stage_dispatch_skipped",
            plan_id=state["plan_id"],
            stage_id=stage.get("id"),
            reason="missing dispatch fields",
        )
        return
    try:
        result = runtime.send_message(
            workspace,
            to,
            str(content),
            sender="orchestrator",
            requires_ack=False,
            wait_visible=False,
        )
    except Exception as exc:
        event_log.write(
            "orchestrator.stage_dispatch_failed",
            plan_id=state["plan_id"],
            stage_id=stage.get("id"),
            error=str(exc),
        )
        return
    event_log.write(
        "orchestrator.stage_dispatched",
        plan_id=state["plan_id"],
        stage_id=stage.get("id"),
        to=to,
        message_id=result.get("message_id"),
        status=result.get("status"),
    )


def _halt_plan(
    workspace: Path,
    state: dict[str, Any],
    stage: dict[str, Any],
    envelope: dict[str, Any],
    halt_reason: str,
    event_log: EventLog,
) -> dict[str, Any]:
    now = datetime.now(timezone.utc)
    ts = now.strftime("%Y%m%dT%H%M%SZ")
    artifact = artifact_path(workspace, state["plan_id"], ts)
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text(_format_halt_artifact(state, stage, envelope, halt_reason, now), encoding="utf-8")
    state["status"] = "halted"
    state["halt_reason"] = halt_reason
    state["halt_artifact"] = str(artifact)
    state["halted_at"] = now.isoformat()
    state["halt_envelope"] = envelope
    save_plan_state(workspace, state)
    event_log.write(
        "orchestrator.plan_halted",
        plan_id=state["plan_id"],
        stage_id=stage.get("id"),
        halt_reason=halt_reason,
        artifact=str(artifact),
    )
    return {
        "ok": True,
        "status": "halted",
        "plan_id": state["plan_id"],
        "current_stage": state["current_stage"],
        "halt_reason": halt_reason,
        "halt_artifact": str(artifact),
    }


def _format_halt_artifact(
    state: dict[str, Any],
    stage: dict[str, Any],
    envelope: dict[str, Any],
    halt_reason: str,
    now: datetime,
) -> str:
    lines = [
        f"# Plan halt: {state['plan_id']}",
        "",
        f"Stage: {stage.get('id', state['current_stage'])}",
        f"Halt reason: {halt_reason}",
        f"Halted at: {now.isoformat()}",
        "",
        "## Stage definition",
        "",
        "```json",
        json.dumps(stage, indent=2, ensure_ascii=False),
        "```",
        "",
        "## Report envelope",
        "",
        "```json",
        json.dumps(envelope, indent=2, ensure_ascii=False),
        "```",
        "",
    ]
    return "\n".join(lines)


__all__ = [
    "halt_plan",
    "handle_report_result",
    "plan_status",
    "resume_plans",
    "start_plan",
]
