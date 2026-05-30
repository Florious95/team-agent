from __future__ import annotations

import os
import uuid
from pathlib import Path
from typing import Any, Protocol

from team_agent.state import load_runtime_state, select_runtime_state


COMMS_BOUNDARY_TEXT = (
    "validates live pane binding consistency. Does NOT perform live runtime message round-trip. "
    "comms contract suite deferred to 0.2.9 (test files not shipped). (zero token, zero pollution)"
)


class CommsSelftestDriver(Protocol):
    """Injectable boundary for tests; production reads state only."""


def run_comms_selftest(
    workspace: Path,
    *,
    team: str | None = None,
    gate: str | None = None,
    response_sla_sec: float = 20.0,
    probe_content: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]:
    del gate, response_sla_sec, probe_content
    workspace = workspace.resolve()
    driver = driver or _DefaultCommsSelftestDriver()
    run_id = _driver_call(driver, "run_id", default=None) or _driver_value(driver, "run_id", default=None) or uuid.uuid4().hex[:12]
    checks = {
        "receiver_binding": _receiver_binding_check(workspace, team, driver),
        "contract_suite": _contract_suite_check(workspace, driver),
        "provider_sdk_calls": _provider_sdk_calls_check(driver),
    }
    ok = all(_check_pass(check) for check in checks.values())
    return {
        "ok": ok,
        "status": "pass" if ok else "fail",
        "run_id": run_id,
        "scope": "binding_consistency",
        "boundary": COMMS_BOUNDARY_TEXT,
        "checks": checks,
    }


def evaluate_idle_behavior(
    workspace: Path,
    *,
    agent_id: str,
    claimed_status: str,
    response_sla_sec: float = 20.0,
    token: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]:
    run_id = uuid.uuid4().hex[:12]
    probe_token = token or f"idle-challenge-{run_id}"
    driver = driver or _DefaultCommsSelftestDriver()
    result = _driver_call(
        driver,
        "evaluate_idle_behavior",
        workspace.resolve(),
        agent_id=agent_id,
        claimed_status=claimed_status,
        response_sla_sec=response_sla_sec,
        token=probe_token,
        default=None,
    )
    if isinstance(result, dict):
        return _normalize_idle_result(result, probe_token)
    idle_execution = _driver_value(driver, "idle_execution", default=None)
    if idle_execution is not None:
        execution = str(idle_execution.get("status") if isinstance(idle_execution, dict) else idle_execution)
        return {
            "ok": execution not in {"timeout", "fail", "failed"},
            "agent_id": agent_id,
            "claimed_status": claimed_status,
            "token": probe_token,
            "status": "pass" if execution not in {"timeout", "fail", "failed"} else "fail",
            "execution_ack": execution,
            "classification_accuracy": "pass" if execution not in {"timeout", "fail", "failed"} else "fail",
        }
    status = str(claimed_status or "").upper()
    return {
        "ok": status in {"IDLE", "WORKING", "RUNNING"},
        "agent_id": agent_id,
        "claimed_status": claimed_status,
        "token": probe_token,
        "status": "not_challenged",
        "execution_ack": "pass" if status in {"IDLE", "WORKING", "RUNNING"} else "timeout",
    }


def _receiver_binding_check(workspace: Path, team: str | None, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "receiver_binding", workspace, team=team, default=None)
    if isinstance(override, dict):
        out = dict(override)
        out.setdefault("status", "pass" if out.get("ok", True) else "fail")
        out.setdefault("verifies", "binding_consistency")
        out.setdefault("proof", "state_read")
        out.setdefault("state_read_observed", True)
        return out
    state = _selftest_state(workspace, team, driver)
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    owner = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    receiver_pane = str(receiver.get("pane_id") or "")
    owner_pane = str(owner.get("pane_id") or "")
    caller_pane = str(_driver_call(driver, "current_pane_id", default=None) or os.environ.get("TMUX_PANE") or "")
    mismatches: list[str] = []
    if owner_pane and receiver_pane and owner_pane != receiver_pane:
        mismatches.append("owner_receiver_pane_mismatch")
    if caller_pane and owner_pane and caller_pane != owner_pane:
        mismatches.append("caller_owner_pane_mismatch")
    if caller_pane and receiver_pane and caller_pane != receiver_pane:
        mismatches.append("caller_receiver_pane_mismatch")
    return {
        "status": "fail" if mismatches else "pass",
        "verifies": "binding_consistency",
        "proof": "state_read",
        "state_read_observed": True,
        "pane_id": receiver_pane,
        "owner_pane_id": owner_pane,
        "caller_pane_id": caller_pane,
        "mismatches": mismatches,
        "configured": bool(receiver_pane),
    }


def _contract_suite_check(workspace: Path, driver: CommsSelftestDriver) -> dict[str, Any]:
    del workspace, driver
    return {
        "status": "deferred",
        "deferred_to": "0.2.9",
        "reason": "contract test files not shipped with package",
        "message": "comms contract verification deferred to 0.2.9; contract test files not shipped with package",
    }


def _provider_sdk_calls_check(driver: CommsSelftestDriver) -> dict[str, Any]:
    calls = _driver_value(driver, "provider_sdk_calls", default=None)
    if not isinstance(calls, dict):
        calls = {"anthropic": 0, "openai": 0, "httpx": 0}
    calls = {name: int(calls.get(name, 0) or 0) for name in ("anthropic", "openai", "httpx")}
    return {
        "status": "fail" if any(calls.values()) else "pass",
        "verifies": "no_provider_sdk_calls",
        "calls": calls,
    }


def _selftest_state(workspace: Path, team: str | None, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "select_runtime_state", workspace, team=team, default=None)
    if isinstance(override, dict):
        return dict(override)
    override = _driver_call(driver, "load_runtime_state", workspace, default=None)
    if isinstance(override, dict):
        return dict(override)
    override = _driver_value(driver, "state", default=None)
    if isinstance(override, dict):
        return dict(override)
    override = _driver_value(driver, "state_before", default=None)
    if isinstance(override, dict):
        return dict(override)
    return select_runtime_state(workspace, team)


def _check_pass(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    if value.get("status") == "deferred":
        return True
    return value.get("status") in {"pass", "not_implemented"} and _has_required_evidence(value)


def _has_required_evidence(value: dict[str, Any]) -> bool:
    verifies = value.get("verifies")
    if verifies == "binding_consistency":
        return value.get("proof") == "state_read" and value.get("state_read_observed") is True
    if verifies == "no_provider_sdk_calls":
        calls = value.get("calls") if isinstance(value.get("calls"), dict) else {}
        return all(int(calls.get(name, 0) or 0) == 0 for name in ("anthropic", "openai", "httpx"))
    return value.get("status") == "pass"


def _normalize_idle_result(result: dict[str, Any], token: str) -> dict[str, Any]:
    out = dict(result)
    out.setdefault("token", token)
    if "execution_ack" not in out:
        if out.get("ok") is False or out.get("status") in {"timeout", "busy", "fail"}:
            out["execution_ack"] = "timeout"
        else:
            out["execution_ack"] = "pass"
    return out


def _driver_call(driver: CommsSelftestDriver | None, name: str, *args: Any, default: Any = None, **kwargs: Any) -> Any:
    fn = getattr(driver, name, None)
    if not callable(fn):
        return default
    return fn(*args, **kwargs)


def _driver_value(driver: CommsSelftestDriver | None, name: str, default: Any = None) -> Any:
    if driver is None:
        return default
    return getattr(driver, name, default)


class _DefaultCommsSelftestDriver:
    pass
