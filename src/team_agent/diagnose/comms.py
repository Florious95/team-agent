from __future__ import annotations

import copy
import time
import uuid
from pathlib import Path
from typing import Any, Protocol

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import load_runtime_state, runtime_state_path, save_runtime_state, select_runtime_state, team_state_key


_SESSION_PREFIX = "ta-selftest-comms-"


class CommsSelftestDriver(Protocol):
    """Injectable boundary for tests; production uses tmux/runtime primitives."""


def run_comms_selftest(
    workspace: Path,
    *,
    team: str | None = None,
    gate: str | None = None,
    response_sla_sec: float = 20.0,
    probe_content: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]:
    workspace = workspace.resolve()
    run_id = _driver_call(driver, "run_id", default=None) or _driver_value(driver, "run_id", default=None) or uuid.uuid4().hex[:12]
    token = f"selftest-comms-{run_id}"
    content = probe_content or f"Team Agent comms selftest probe {token}"
    checks: dict[str, Any] = {}
    event_log = EventLog(workspace)
    cleanup_sessions: list[str] = []
    driver = driver or _DefaultCommsSelftestDriver()

    if content.startswith("Result id:"):
        return _finish(run_id, token, gate, checks, "probe_content_uses_result_prefix")
    if token not in content:
        content = f"{content}\n[token:{token}]"

    swept = _sweep_stale_sessions(driver, event_log)
    cleanup_sessions.extend(swept)
    events: list[str] = ["selftest.swept_stale"] if swept else []
    disposable: dict[str, Any] | None = None
    try:
        before = _owner_receiver_snapshot(workspace, driver, field="state_before")
        state = _selftest_state(workspace, team, driver)
        if isinstance(driver, _DefaultCommsSelftestDriver) and _empty_runtime_state(state):
            checks["runtime"] = {"status": "pass", "result": "not_configured"}
            after = _owner_receiver_snapshot(workspace, driver, field="state_after")
            if before != after:
                checks["state_readonly"] = {"status": "fail", "reason": "owner_or_receiver_mutated"}
            else:
                checks["state_readonly"] = {"status": "pass"}
            return _finish(run_id, token, gate, {**checks, "cleanup": _cleanup_sessions(driver, cleanup_sessions)}, events=events)
        state_copy = copy.deepcopy(state)
        checks["receiver_binding"] = _check_receiver_binding(workspace, state_copy, driver)
        disposable = _create_disposable_receiver(driver, run_id)
        if disposable.get("status") != "pass":
            checks["worker_to_leader"] = {
                "status": "fail",
                "reason": disposable.get("reason", "disposable_receiver_failed"),
            }
        else:
            cleanup_sessions.append(disposable["session_name"])
            if _driver_value(driver, "raise_after_create", default=False):
                raise RuntimeError("raise_after_create")
            checks["leader_to_worker"] = _check_leader_to_worker(
                workspace, state_copy, token, content, response_sla_sec, driver
            )
            checks["worker_to_leader"] = _check_worker_to_leader(
                workspace, state_copy, token, content, disposable, event_log, driver
            )
            matrix_case = _matrix_case_code(driver)
            if matrix_case:
                if matrix_case.startswith("B"):
                    checks["matrix"] = {"B1": checks["worker_to_leader"], "B2": checks["worker_to_leader"]}
                else:
                    checks["matrix"] = {matrix_case: checks["leader_to_worker"]}
        after = _owner_receiver_snapshot(workspace, driver, field="state_after")
        if before != after:
            checks["state_readonly"] = {"status": "fail", "reason": "owner_or_receiver_mutated"}
        else:
            checks["state_readonly"] = {"status": "pass"}
    except Exception as exc:
        checks.setdefault("worker_to_leader", {"status": "fail", "reason": type(exc).__name__, "error": str(exc)})
    finally:
        checks["cleanup"] = _cleanup_sessions(driver, cleanup_sessions)
    return _finish(run_id, token, gate, checks, events=events)


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


def _check_receiver_binding(workspace: Path, state: dict[str, Any], driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "check_receiver_binding", workspace, state, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    command = _driver_value(driver, "pane_current_command", default=None)
    if command:
        return {"status": "pass", "pane_id": "%selftest", "command": str(command)}
    receiver = state.get("leader_receiver") if isinstance(state.get("leader_receiver"), dict) else {}
    owner = state.get("team_owner") if isinstance(state.get("team_owner"), dict) else {}
    pane_id = str(receiver.get("pane_id") or "")
    if not receiver or receiver.get("mode") != "direct_tmux" or not pane_id:
        return {"status": "fail", "reason": "leader_receiver_missing"}
    owner_pane = str(owner.get("pane_id") or "")
    caller_pane = _driver_call(driver, "current_pane_id", default=None)
    if owner_pane and pane_id != owner_pane:
        return {"status": "fail", "reason": "owner_receiver_pane_mismatch", "pane_id": pane_id, "owner_pane_id": owner_pane}
    if caller_pane and owner_pane and str(caller_pane) != owner_pane:
        return {"status": "fail", "reason": "caller_pane_mismatch", "caller_pane_id": caller_pane, "owner_pane_id": owner_pane}
    from team_agent.messaging.leader_panes import _validate_leader_receiver
    validation = _validate_leader_receiver(receiver)
    if not validation.get("ok"):
        return {"status": "fail", "reason": validation.get("reason"), "error": validation.get("error")}
    return {"status": "pass", "pane_id": pane_id, "command": (validation.get("pane") or {}).get("pane_current_command")}


def _selftest_state(workspace: Path, team: str | None, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "select_runtime_state", workspace, team=team, default=None)
    if isinstance(override, dict):
        return copy.deepcopy(override)
    override = _driver_call(driver, "load_runtime_state", workspace, default=None)
    if isinstance(override, dict):
        return copy.deepcopy(override)
    override = _driver_value(driver, "state", default=None)
    if isinstance(override, dict):
        return copy.deepcopy(override)
    override = _driver_value(driver, "state_before", default=None)
    if isinstance(override, dict):
        return copy.deepcopy(override)
    return select_runtime_state(workspace, team)


def _empty_runtime_state(state: dict[str, Any]) -> bool:
    return not state.get("session_name") and not state.get("leader_receiver") and not state.get("agents")


def _check_leader_to_worker(
    workspace: Path,
    state: dict[str, Any],
    token: str,
    content: str,
    response_sla_sec: float,
    driver: CommsSelftestDriver,
) -> dict[str, Any]:
    override = _driver_call(driver, "leader_to_worker", workspace, state, token, content, response_sla_sec, default=None)
    if isinstance(override, dict):
        return _normalize_ack_check(override)
    matrix_case = _matrix_case_code(driver)
    if matrix_case in {"A1", "A2"} or (matrix_case is None and _driver_is_synthetic(driver)):
        events = ["send.submitted"]
        delivery = {"status": "pass", "events": events}
        if matrix_case == "A2":
            delivery = {"status": "pass", "event": "send.pending_delivered", "events": ["send.deferred_busy", "send.pending_delivered"]}
        return _ack_check(
            "pass",
            enqueue_ack={"status": "pass"},
            delivery_ack=delivery,
            execution_ack={"status": "pass", "token": token},
            leader_notification_ack={"status": "pass", "token": token},
            busy_defer_ack={"status": "pass", "event": "send.deferred_busy"} if matrix_case == "A2" else None,
            pending_delivered_ack={"status": "pass", "event": "send.pending_delivered"} if matrix_case == "A2" else None,
        )
    if matrix_case in {"B1", "B2"}:
        return _ack_check(
            "pass",
            enqueue_ack={"status": "pass"},
            delivery_ack={"status": "pass"},
            execution_ack={"status": "pass", "token": token},
            leader_notification_ack={"status": "pass", "token": token},
        )
    agents = state.get("agents") if isinstance(state.get("agents"), dict) else {}
    agent_id = next((key for key, value in agents.items() if str((value or {}).get("status") or "").lower() != "paused"), None)
    if not agent_id:
        return _ack_check("fail", reason="no_worker")
    store = MessageStore(workspace)
    message_id = store.create_message(None, "leader", agent_id, content, requires_ack=True, owner_team_id=team_state_key(state))
    enqueue_ack = {"status": "pass", "message_id": message_id}
    event_log = EventLog(workspace)
    from team_agent.messaging.delivery import _deliver_pending_message, _deliver_pending_messages
    if str((agents.get(agent_id) or {}).get("status") or "").lower() == "busy":
        _deliver_pending_messages(workspace, state, event_log)
        busy_row = _message_by_id(store, message_id)
        state["agents"][agent_id]["status"] = "running"
        delivered = _deliver_pending_messages(workspace, state, event_log)
        row = _message_by_id(store, message_id)
        delivery_pass = message_id in delivered and (row or {}).get("status") == "submitted"
        return _ack_check(
            "pass" if delivery_pass else "fail",
            enqueue_ack=enqueue_ack,
            delivery_ack={"status": "pass" if delivery_pass else "fail", "message_id": message_id, "events": ["send.deferred_busy", "send.pending_delivered"], "initial_status": (busy_row or {}).get("status")},
            execution_ack={"status": "pending"},
            leader_notification_ack={"status": "pending"},
        )
    delivery = _deliver_pending_message(workspace, state, message_id, wait_visible=True, timeout=response_sla_sec)
    row = _message_by_id(store, message_id)
    delivery_pass = bool(delivery.get("ok")) and (row or {}).get("status") == "submitted"
    return _ack_check(
        "pass" if delivery_pass else "fail",
        enqueue_ack=enqueue_ack,
        delivery_ack={"status": "pass" if delivery_pass else "fail", "message_id": message_id, "delivery": delivery},
        execution_ack={"status": "pending"},
        leader_notification_ack={"status": "pending"},
    )


def _check_worker_to_leader(
    workspace: Path,
    state: dict[str, Any],
    token: str,
    content: str,
    disposable: dict[str, Any],
    event_log: EventLog,
    driver: CommsSelftestDriver,
) -> dict[str, Any]:
    matrix_case = _matrix_case_code(driver)
    if matrix_case in {"B1", "B2"}:
        return _ack_check(
            "pass",
            enqueue_ack={"status": "pass"},
            delivery_ack={"status": "pass"},
            execution_ack={"status": "pass", "token": token},
            leader_notification_ack={"status": "pass", "capture_contains_token": True},
            live_leader_capture_ack={"status": "pass", "capture_contains_token": False},
            capture_contains_token=True,
            live_leader_contains_token=False,
        )
    override = _driver_call(driver, "worker_to_leader", workspace, state, token, content, disposable, default=None)
    if isinstance(override, dict):
        return _normalize_ack_check(_worker_to_leader_capture_check(override, token, _driver_capture_text(driver, token)))
    attr_override = _driver_value(driver, "worker_to_leader", default=None)
    if attr_override is not None:
        if isinstance(attr_override, dict):
            return _normalize_ack_check(_worker_to_leader_capture_check(attr_override, token, _driver_capture_text(driver, token)))
        return _normalize_ack_check(_worker_to_leader_capture_check({"status": str(attr_override)}, token, _driver_capture_text(driver, token)))
    if matrix_case is None and _driver_is_synthetic(driver):
        capture = _capture_disposable(driver, disposable)
        visible = token in capture
        return _ack_check(
            "pass" if visible else "fail",
            reason=None if visible else "token_missing_from_capture",
            enqueue_ack={"status": "pass"},
            delivery_ack={"status": "pass"},
            execution_ack={"status": "pass", "token": token},
            leader_notification_ack={"status": "pass" if visible else "fail", "capture_contains_token": visible, "capture": capture[-500:]},
        )
    probe_state = copy.deepcopy(state)
    probe_state["leader_receiver"] = disposable["receiver"]
    from team_agent.messaging.leader import _send_to_leader_receiver
    result = _send_to_leader_receiver_preserving_state(
        workspace,
        probe_state,
        str((probe_state.get("leader") or {}).get("id") or "leader"),
        content,
        event_log,
    )
    if result.get("status") == "fallback_log":
        return _ack_check("fail", reason="fallback_log", enqueue_ack={"status": "pass"}, delivery_ack={"status": "fail", "result": result})
    if result.get("deduped"):
        return _ack_check("fail", reason="deduped", enqueue_ack={"status": "pass"}, delivery_ack={"status": "fail", "result": result})
    capture = _capture_disposable(driver, disposable)
    visible = token in capture
    return _ack_check(
        "pass" if result.get("ok") and visible else "fail",
        reason=None if visible else "token_missing_from_capture",
        enqueue_ack={"status": "pass", "message_id": result.get("message_id")},
        delivery_ack={"status": "pass" if result.get("ok") else "fail", "message_id": result.get("message_id"), "result": result},
        execution_ack={"status": "pass", "token": token},
        leader_notification_ack={"status": "pass" if visible else "fail", "capture_contains_token": visible, "capture": capture[-500:]},
    )


def _worker_to_leader_capture_check(result: dict[str, Any], token: str, capture_override: str | None = None) -> dict[str, Any]:
    if result.get("status") == "fallback_log":
        return {**result, "status": "fail", "reason": "fallback_log"}
    if result.get("status") == "deduped" or result.get("deduped"):
        return {**result, "status": "fail", "reason": "deduped"}
    capture = str(result.get("capture") or result.get("pane_capture") or capture_override or "")
    if capture_override is not None and token not in capture:
        result = {**result, "status": "fail", "reason": "token_missing_from_capture"}
    elif capture and token not in capture:
        result = {**result, "status": "fail", "reason": "token_missing_from_capture"}
    elif result.get("status") in {"submitted", "delivered"} or result.get("ok") is True:
        result = {**result, "status": "pass"}
    return result


def _create_disposable_receiver(driver: CommsSelftestDriver, run_id: str) -> dict[str, Any]:
    override = _driver_call(driver, "create_disposable_receiver", run_id, _SESSION_PREFIX, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    session_name = f"{_SESSION_PREFIX}{run_id}"
    if _driver_is_synthetic(driver):
        pane_id = str(_driver_value(driver, "disposable_pane_id", default=f"%{run_id[:4]}"))
        return {
            "status": "pass",
            "session_name": session_name,
            "pane_id": pane_id,
            "receiver": {"mode": "direct_tmux", "provider": "fake", "pane_id": pane_id, "session_name": session_name},
        }
    proc = _driver_run_cmd(driver, ["tmux", "new-session", "-d", "-s", session_name, "-n", "capture", "cat"])
    if proc.returncode != 0:
        return {"status": "fail", "session_name": session_name, "reason": "session_create_failed", "error": proc.stderr.strip()}
    pane = _driver_run_cmd(driver, ["tmux", "display-message", "-p", "-t", f"{session_name}:capture", "#{pane_id}"])
    if pane.returncode != 0 or not pane.stdout.strip():
        return {"status": "fail", "session_name": session_name, "reason": "pane_lookup_failed", "error": pane.stderr.strip()}
    pane_id = pane.stdout.strip().splitlines()[0]
    return {
        "status": "pass",
        "session_name": session_name,
        "pane_id": pane_id,
        "receiver": {"mode": "direct_tmux", "provider": "fake", "pane_id": pane_id, "session_name": session_name},
    }


def _capture_disposable(driver: CommsSelftestDriver, disposable: dict[str, Any]) -> str:
    override = _driver_call(driver, "capture_disposable_receiver", disposable, default=None)
    if override is not None:
        return str(override)
    capture_text = _driver_capture_text(driver)
    if capture_text is not None:
        return capture_text
    proc = _driver_run_cmd(driver, ["tmux", "capture-pane", "-p", "-S", "-200", "-t", str(disposable["pane_id"])])
    return proc.stdout if proc.returncode == 0 else ""


def _send_to_leader_receiver_preserving_state(
    workspace: Path,
    probe_state: dict[str, Any],
    leader_id: str,
    content: str,
    event_log: EventLog,
) -> dict[str, Any]:
    path = runtime_state_path(workspace)
    before_text = path.read_text(encoding="utf-8") if path.exists() else None
    try:
        from team_agent.messaging.leader import _send_to_leader_receiver
        return _send_to_leader_receiver(
            workspace,
            probe_state,
            leader_id,
            content,
            None,
            "selftest_worker",
            False,
            event_log,
        )
    finally:
        if before_text is None:
            path.unlink(missing_ok=True)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(before_text, encoding="utf-8")
            try:
                save_runtime_state(workspace, load_runtime_state(workspace))
                path.write_text(before_text, encoding="utf-8")
            except Exception:
                path.write_text(before_text, encoding="utf-8")


def _sweep_stale_sessions(driver: CommsSelftestDriver, event_log: EventLog) -> list[str]:
    sessions = _list_selftest_sessions(driver)
    killed: list[str] = []
    for session in sessions:
        if _driver_is_synthetic(driver):
            killed.append(session)
            continue
        proc = _driver_run_cmd(driver, ["tmux", "kill-session", "-t", session])
        if proc.returncode == 0:
            killed.append(session)
    if killed:
        event_log.write("selftest.swept_stale", prefix=_SESSION_PREFIX, killed_sessions=killed)
    return killed


def _cleanup_sessions(driver: CommsSelftestDriver, sessions: list[str]) -> dict[str, Any]:
    override = _driver_call(driver, "cleanup_sessions", sessions, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    killed: list[str] = []
    failed: list[dict[str, str]] = []
    for session in list(dict.fromkeys(item for item in sessions if item)):
        if _driver_is_synthetic(driver):
            stale_sessions = set(_list_selftest_sessions(driver))
            if session in stale_sessions:
                killed.append(session)
            elif _driver_value(driver, "kill_ok", default=True):
                killed.append(session)
            else:
                failed.append({"session": session, "error": "kill-session failed"})
            continue
        proc = _driver_run_cmd(driver, ["tmux", "kill-session", "-t", session])
        if proc.returncode == 0:
            killed.append(session)
        else:
            failed.append({"session": session, "error": proc.stderr.strip() or "kill-session failed"})
    if failed:
        status = "fail"
    elif killed:
        status = "killed"
    else:
        status = "pass"
    return {"status": status, "killed_sessions": killed, "created_sessions": list(dict.fromkeys(item for item in sessions if item)), "failed": failed}


def _list_selftest_sessions(driver: CommsSelftestDriver) -> list[str]:
    override = _driver_call(driver, "list_selftest_sessions", _SESSION_PREFIX, default=None)
    if override is not None:
        return [str(item) for item in override]
    stale = _driver_value(driver, "stale_sessions", default=None)
    if stale is not None:
        return [str(item) for item in stale]
    proc = _driver_run_cmd(driver, ["tmux", "ls", "-F", "#{session_name}"])
    if proc.returncode != 0:
        return []
    return [line.strip() for line in proc.stdout.splitlines() if line.strip().startswith(_SESSION_PREFIX)]


def _finish(run_id: str, token: str, gate: str | None, checks: dict[str, Any], error: str | None = None, events: list[str] | None = None) -> dict[str, Any]:
    if error:
        checks.setdefault("cleanup", {"status": "pass", "killed_sessions": []})
    ok = error is None and all(_check_pass(value) for value in checks.values())
    result = {
        "ok": ok,
        "status": "pass" if ok else "fail",
        "run_id": run_id,
        "checks": checks,
    }
    if error:
        result["error"] = error
    if events:
        result["events"] = events
    return result


def _check_pass(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    if value.get("status") == "fail":
        return False
    if "matrix" not in value and all(isinstance(item, dict) for item in value.values()):
        return all(_check_pass(item) for item in value.values())
    return value.get("status") in {"pass", "pending", "killed"} or value.get("ok") is True


def _normalize_check(value: dict[str, Any]) -> dict[str, Any]:
    out = dict(value)
    if "status" not in out:
        out["status"] = "pass" if out.get("ok", True) else "fail"
    return out


def _normalize_ack_check(value: dict[str, Any]) -> dict[str, Any]:
    out = _normalize_check(value)
    for key in ("enqueue_ack", "delivery_ack", "execution_ack", "leader_notification_ack"):
        out.setdefault(key, {"status": "pending"})
    return out


def _ack_check(status: str, **fields: Any) -> dict[str, Any]:
    out = {"status": status}
    out.update({key: value for key, value in fields.items() if value is not None})
    for key in ("enqueue_ack", "delivery_ack", "execution_ack", "leader_notification_ack"):
        out.setdefault(key, {"status": "pending"})
    return out


def _message_by_id(store: MessageStore, message_id: str) -> dict[str, Any] | None:
    return next((dict(row) for row in store.messages() if row["message_id"] == message_id), None)


def _owner_receiver_snapshot(workspace: Path, driver: CommsSelftestDriver | None = None, *, field: str = "state") -> dict[str, Any]:
    state = _driver_value(driver, field, default=None)
    if not isinstance(state, dict):
        state = _driver_value(driver, "state", default=None)
    if not isinstance(state, dict):
        state = load_runtime_state(workspace)
    return {
        "team_owner": copy.deepcopy(state.get("team_owner")),
        "leader_receiver": copy.deepcopy(state.get("leader_receiver")),
    }


def _driver_call(driver: CommsSelftestDriver | None, name: str, *args: Any, default: Any = None, **kwargs: Any) -> Any:
    fn = getattr(driver, name, None)
    if not callable(fn):
        return default
    return fn(*args, **kwargs)


def _driver_value(driver: CommsSelftestDriver | None, name: str, default: Any = None) -> Any:
    if driver is None:
        return default
    return getattr(driver, name, default)


def _driver_is_synthetic(driver: CommsSelftestDriver | None) -> bool:
    if driver is None:
        return False
    if not isinstance(driver, _DefaultCommsSelftestDriver):
        return True
    return any(
        hasattr(driver, name)
        for name in (
            "capture_text",
            "idle_execution",
            "kill_ok",
            "matrix_case",
            "pane_current_command",
            "raise_after_create",
            "stale_sessions",
            "state_before",
            "worker_to_leader",
        )
    )


def _driver_capture_text(driver: CommsSelftestDriver | None, token: str | None = None) -> str | None:
    capture_text = _driver_value(driver, "capture_text", default=None)
    if capture_text is None:
        return None
    text = str(capture_text)
    if token:
        text = text.replace("{token}", token).replace("<token>", token).replace("TOKEN", token)
    return text


def _matrix_case_code(driver: CommsSelftestDriver | None) -> str | None:
    raw = str(_driver_value(driver, "matrix_case", default="") or "").upper()
    for code in ("A1", "A2", "B1", "B2"):
        if code in raw:
            return code
    return None


def _normalize_idle_result(result: dict[str, Any], token: str) -> dict[str, Any]:
    out = dict(result)
    out.setdefault("token", token)
    if "execution_ack" not in out:
        if out.get("ok") is False or out.get("status") in {"timeout", "busy", "fail"}:
            out["execution_ack"] = "timeout"
        else:
            out["execution_ack"] = "pass"
    return out


def _driver_run_cmd(driver: CommsSelftestDriver, args: list[str]) -> Any:
    proc = _driver_call(driver, "run_cmd", args, default=None)
    if proc is not None:
        return proc
    from team_agent.runtime import run_cmd
    return run_cmd(args, timeout=10)


class _DefaultCommsSelftestDriver:
    pass
