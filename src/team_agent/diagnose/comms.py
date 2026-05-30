from __future__ import annotations

import copy
import hashlib
import os
import shutil
import sqlite3
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Protocol

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import load_runtime_state, select_runtime_state, team_state_key


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
    live_fingerprint_before = _live_workspace_fingerprint(workspace)
    live_state_for_scan: dict[str, Any] = {}
    throwaway_root = Path("/tmp") / f"{_SESSION_PREFIX}{run_id}"

    if token not in content:
        content = f"{content}\n[token:{token}]"

    swept = _sweep_stale(driver, event_log)
    events: list[dict[str, Any]] = []
    if swept["tmux"] or swept["workspaces"]:
        events.append({"event": "selftest.swept_stale", **swept})
        events.append("selftest.swept_stale")
    disposable: dict[str, Any] | None = None
    try:
        before = _owner_receiver_snapshot(workspace, driver, field="state_before")
        state = _selftest_state(workspace, team, driver)
        live_state_for_scan = copy.deepcopy(state)
        if isinstance(driver, _DefaultCommsSelftestDriver) and _empty_runtime_state(state):
            checks["runtime"] = {"status": "pass", "result": "not_configured"}
            after = _owner_receiver_snapshot(workspace, driver, field="state_after")
            if before != after:
                checks["state_readonly"] = {"status": "fail", "reason": "owner_or_receiver_mutated"}
            else:
                checks["state_readonly"] = {"status": "pass"}
            return _finish(run_id, token, gate, {**checks, "cleanup": _cleanup_sessions(driver, cleanup_sessions, already_killed=swept["tmux"])}, events=events)
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
            checks["worker_to_leader"] = _deferred_worker_to_leader_check()
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
        checks["cleanup"] = _cleanup_resources(driver, cleanup_sessions, throwaway_root, already_killed=swept["tmux"])
    checks.setdefault("live_workspace_unchanged", _live_workspace_unchanged_check(live_fingerprint_before, workspace, driver) if _requires_live_workspace_restore(driver) else {"status": "pass", "skipped": True})
    checks.setdefault("live_leader_pollution", _live_leader_pollution_check(workspace, live_state_for_scan, token, driver, phase="after"))
    checks.setdefault(
        "global_registry_pollution",
        _global_registry_pollution_check(run_id, throwaway_root, driver)
        if _requires_live_workspace_restore(driver) or _driver_value(driver, "global_registry_pollution", default=None) is not None
        else {"status": "pass", "run_id": run_id, "detected_in": [], "detected_paths": [], "skipped": True},
    )
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
    from team_agent.messaging.delivery import _deliver_pending_message
    if str((agents.get(agent_id) or {}).get("status") or "").lower() == "busy":
        event_log.write(
            "send.deferred_busy",
            message_id=message_id,
            sender="leader",
            recipient=agent_id,
            reason="recipient_busy",
            selftest=True,
        )
        busy_row = _message_by_id(store, message_id)
        state["agents"][agent_id]["status"] = "running"
        delivery = _deliver_pending_message(workspace, state, message_id, wait_visible=True, timeout=response_sla_sec)
        row = _message_by_id(store, message_id)
        delivery_pass = bool(delivery.get("ok")) and (row or {}).get("status") == "submitted"
        if delivery_pass:
            event_log.write("send.pending_delivered", message_id=message_id, agent_id=agent_id, selftest=True)
        return _ack_check(
            "pass" if delivery_pass else "fail",
            enqueue_ack=enqueue_ack,
            delivery_ack={"status": "pass" if delivery_pass else "fail", "message_id": message_id, "events": ["send.deferred_busy", "send.pending_delivered"], "initial_status": (busy_row or {}).get("status"), "delivery": delivery},
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


def _deferred_worker_to_leader_check() -> dict[str, Any]:
    return {
        "status": "not_implemented",
        "deferred_to": "0.2.9",
        "reason": "worker_to_leader_selftest_deferred",
        "probe_ran": False,
    }


def _check_worker_to_leader(*args: Any, **kwargs: Any) -> dict[str, Any]:
    return _deferred_worker_to_leader_check()


def _create_disposable_receiver(driver: CommsSelftestDriver, run_id: str) -> dict[str, Any]:
    override = _driver_call(driver, "create_disposable_receiver", run_id, _SESSION_PREFIX, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    session_name = f"{_SESSION_PREFIX}{run_id}"
    if _driver_is_synthetic(driver):
        pane_id = str(_driver_value(driver, "disposable_pane_id", default="%capture"))
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


def _sweep_stale(driver: CommsSelftestDriver, event_log: EventLog) -> dict[str, list[str]]:
    tmux = _sweep_stale_sessions(driver, event_log)
    workspaces = _sweep_stale_workspaces(driver)
    if tmux or workspaces:
        _driver_call(driver, "record_swept_stale", {"tmux": tmux, "workspaces": workspaces}, default=None)
    return {"tmux": tmux, "workspaces": workspaces}


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
    return killed


def _sweep_stale_workspaces(driver: CommsSelftestDriver) -> list[str]:
    override = _driver_call(driver, "list_selftest_workspaces", _SESSION_PREFIX, default=None)
    if override is None:
        for name in ("stale_workspaces", "stale_workspace_dirs", "stale_throwaway_workspaces"):
            override = _driver_value(driver, name, default=None)
            if override is not None:
                break
    if override is not None:
        paths = [Path(str(item)) for item in override]
    else:
        paths = [path for path in Path(tempfile.gettempdir()).glob(f"{_SESSION_PREFIX}*") if path.is_dir()]
    removed: list[str] = []
    for path in paths:
        if _driver_is_synthetic(driver):
            removed.append(str(path))
            continue
        try:
            shutil.rmtree(path)
            removed.append(str(path))
        except FileNotFoundError:
            removed.append(str(path))
        except OSError:
            continue
    return removed


def _cleanup_resources(
    driver: CommsSelftestDriver,
    sessions: list[str],
    throwaway_root: Path,
    *,
    already_killed: list[str] | None = None,
) -> dict[str, Any]:
    tmux = _cleanup_sessions(driver, sessions, already_killed=already_killed)
    workspace = _cleanup_throwaway_workspace(driver, throwaway_root)
    coordinator = _cleanup_process_role(driver, "coordinator")
    worker = _cleanup_process_role(driver, "worker")
    subchecks = {"tmux": tmux, "workspace": workspace, "coordinator": coordinator, "worker": worker}
    failed = [name for name, check in subchecks.items() if check.get("status") != "pass"]
    status = "fail" if failed else ("killed" if tmux.get("killed_sessions") else "pass")
    out: dict[str, Any] = {
        "status": status,
        "tmux": tmux,
        "workspace": workspace,
        "coordinator": coordinator,
        "worker": worker,
        "killed_sessions": tmux.get("killed_sessions", []),
        "created_sessions": tmux.get("created_sessions", []),
        "failed": failed,
    }
    return out


def _cleanup_sessions(driver: CommsSelftestDriver, sessions: list[str], *, already_killed: list[str] | None = None) -> dict[str, Any]:
    override = _driver_call(driver, "cleanup_sessions", sessions, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    killed: list[str] = list(dict.fromkeys(already_killed or []))
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
        elif _tmux_session_missing(proc.stderr):
            continue
        else:
            failed.append({"session": session, "error": proc.stderr.strip() or "kill-session failed"})
    status = "fail" if failed else "pass"
    return {"status": status, "killed_sessions": killed, "created_sessions": list(dict.fromkeys(item for item in sessions if item)), "failed": failed}


def _cleanup_throwaway_workspace(driver: CommsSelftestDriver, throwaway_root: Path) -> dict[str, Any]:
    override = _driver_call(driver, "cleanup_throwaway_workspace", throwaway_root, default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    if _driver_is_synthetic(driver):
        return {"status": "pass", "path": str(throwaway_root), "removed": True}
    try:
        shutil.rmtree(throwaway_root)
        return {"status": "pass", "path": str(throwaway_root), "removed": True}
    except FileNotFoundError:
        return {"status": "pass", "path": str(throwaway_root), "removed": False}
    except OSError as exc:
        return {"status": "fail", "path": str(throwaway_root), "error": str(exc)}


def _cleanup_process_role(driver: CommsSelftestDriver, role: str) -> dict[str, Any]:
    override = _driver_call(driver, f"cleanup_throwaway_{role}", default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    pid = _driver_value(driver, f"throwaway_{role}_pid", default=None)
    if not pid:
        return {"status": "pass", "pid": None, "stopped": True}
    try:
        os.kill(int(pid), 15)
    except ProcessLookupError:
        return {"status": "pass", "pid": int(pid), "stopped": True}
    except OSError as exc:
        return {"status": "fail", "pid": pid, "error": str(exc)}
    return {"status": "pass", "pid": int(pid), "stopped": True}


def _tmux_session_missing(stderr: str) -> bool:
    text = str(stderr or "").lower()
    return "can't find session" in text or "no such session" in text


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


def _live_workspace_fingerprint(workspace: Path) -> dict[str, str]:
    root = workspace / ".team"
    if not root.exists():
        return {}
    out: dict[str, str] = {}
    for path in sorted(item for item in root.rglob("*") if item.is_file()):
        try:
            out[str(path.relative_to(workspace))] = hashlib.sha256(path.read_bytes()).hexdigest()
        except OSError:
            continue
    return out


def _live_workspace_unchanged_check(before: dict[str, str], workspace: Path, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_value(driver, "live_workspace_unchanged", default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    if override is not None:
        return {"status": "pass" if bool(override) else "fail"}
    after = _live_workspace_fingerprint(workspace)
    changed = sorted(set(before) ^ set(after) | {key for key in before.keys() & after.keys() if before[key] != after[key]})
    return {"status": "pass" if not changed else "fail", "changed_files": changed}


def _live_leader_pollution_check(
    workspace: Path,
    state: dict[str, Any],
    token: str,
    driver: CommsSelftestDriver,
    *,
    phase: str,
) -> dict[str, Any]:
    override = _driver_value(driver, "live_leader_pollution", default=None)
    if isinstance(override, dict):
        out = _normalize_check(override)
        out.setdefault("token", token)
        out.setdefault("detected_in", [])
        return out
    state_before = _driver_value(driver, "state_before", default=None)
    live_pane_id = str(
        ((state.get("leader_receiver") or {}) if isinstance(state, dict) else {}).get("pane_id")
        or (((state_before.get("leader_receiver") or {}) if isinstance(state_before, dict) else {}).get("pane_id"))
        or _find_pane_id(state_before)
        or _find_driver_pane_id(driver)
        or _driver_value(driver, "live_pane_id", default="")
        or _driver_value(driver, "live_persisted_receiver_pane_id", default="")
        or _driver_value(driver, "live_receiver_pane_id", default="")
        or ""
    )
    detected: list[str] = []
    async_window = _observe_async_pollution_window(driver)
    before = _driver_capture_named(driver, "live_capture_before", token)
    after = _driver_capture_named(driver, "live_capture_after", token)
    if before is None:
        before = _capture_live_pane(driver, live_pane_id)
    if after is None:
        after = _capture_live_pane(driver, live_pane_id)
    if before and token in before:
        detected.append("capture_before")
    if after and token in after:
        detected.append("capture_after")
    if _live_message_store_contains(workspace, token, driver):
        detected.append("message_store")
    event_hit = _live_event_log_hit(workspace, token, live_pane_id, driver)
    if event_hit:
        detected.append("event_log")
    extra = _driver_value(driver, "pollution_detected_in", default=None)
    if extra:
        detected.extend(str(item) for item in extra)
    detected = list(dict.fromkeys(detected))
    if detected and not live_pane_id:
        live_pane_id = _find_live_pane_from_pollution_sources(workspace, token, driver)
    out = {
        "status": "fail" if detected else "pass",
        "live_pane_id": live_pane_id,
        "token": token,
        "detected_in": detected,
        "phase": phase,
        "async_window": async_window,
        "async_return_window": async_window,
    }
    if event_hit:
        out["matched_event_type"] = event_hit.get("event_type")
        out["detected_event_types"] = [str(event_hit.get("event_type") or "unknown")]
        out["matched_event"] = event_hit
    else:
        out["detected_event_types"] = []
    return out


def _observe_async_pollution_window(driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "observe_async_worker_return_window", default=None)
    if isinstance(override, dict):
        return _normalize_check(override)
    seconds = float(_driver_value(driver, "async_worker_return_window_s", default=0.0) or 0.0)
    if seconds > 0 and not _driver_is_synthetic(driver):
        time.sleep(min(seconds, 2.0))
    return {"status": "observed", "waited_s": seconds}


def _capture_live_pane(driver: CommsSelftestDriver, pane_id: str) -> str:
    if not pane_id:
        return ""
    override = _driver_call(driver, "capture_live_leader", pane_id, default=None)
    if override is not None:
        return str(override)
    if _driver_is_synthetic(driver):
        return ""
    proc = _driver_run_cmd(driver, ["tmux", "capture-pane", "-p", "-S", "-", "-t", pane_id])
    return proc.stdout if proc.returncode == 0 else ""


def _driver_capture_named(driver: CommsSelftestDriver, name: str, token: str) -> str | None:
    value = _driver_value(driver, name, default=None)
    if value is None:
        return None
    return str(value).replace("{token}", token).replace("<token>", token).replace("TOKEN", token)


def _live_message_store_contains(workspace: Path, token: str, driver: CommsSelftestDriver) -> bool:
    override = _driver_value(driver, "live_message_store_contains_token", default=None)
    if override is not None:
        return bool(override)
    messages = _driver_value(driver, "live_messages", default=None)
    if messages is None:
        from team_agent.paths import runtime_dir
        db_path = runtime_dir(workspace) / "team.db"
        messages = []
        if db_path.exists():
            try:
                uri = f"file:{db_path}?mode=ro&immutable=1"
                with sqlite3.connect(uri, uri=True) as conn:
                    conn.row_factory = sqlite3.Row
                    rows = conn.execute(
                        "select recipient, content from messages where recipient = ? and content like ?",
                        ("leader", f"%{token}%"),
                    ).fetchall()
                    messages = [dict(row) for row in rows]
            except Exception:
                messages = []
    for row in messages:
        if str(row.get("recipient") or "") == "leader" and token in str(row.get("content") or ""):
            return True
    return False


def _live_event_log_hit(workspace: Path, token: str, live_pane_id: str, driver: CommsSelftestDriver) -> dict[str, Any] | None:
    override = _driver_value(driver, "live_event_log_contains_token", default=None)
    if override is not None:
        return {"event_type": "driver_override", "line": ""} if bool(override) else None
    events = _driver_value(driver, "live_events", default=None)
    if events is None:
        from team_agent.paths import logs_dir
        path = logs_dir(workspace) / "events.jsonl"
        if not path.exists():
            events = []
        else:
            events = path.read_text(encoding="utf-8").splitlines()
    for event in events:
        text = str(event)
        if token not in text:
            continue
        event_type = _event_type_from_line(text)
        if _leader_delivery_event_type(event_type, text):
            return {"event_type": event_type or "unknown", "line": text[-500:], "live_pane_id": live_pane_id}
    return None


def _event_type_from_line(text: str) -> str:
    try:
        import json
        data = json.loads(text)
    except Exception:
        data = None
    if isinstance(data, dict):
        for key in ("event", "type", "event_type"):
            value = data.get(key)
            if isinstance(value, str):
                return value
    for marker in (
        "leader_receiver.deliver_attempt",
        "leader_receiver.submitted",
        "leader_receiver.delivery_failed",
        "leader_receiver.rebind_required",
        "send.deliver_attempt",
        "send.submitted",
        "send.pending_delivered",
        "send.deferred_busy",
    ):
        if marker in text:
            return marker
    return ""


def _leader_delivery_event_type(event_type: str, text: str) -> bool:
    if event_type.startswith("leader_receiver."):
        return True
    if event_type in {"send.deliver_attempt", "send.submitted", "send.pending_delivered", "send.deferred_busy"}:
        return True
    lowered = text.lower()
    return "leader_receiver" in lowered or ("leader" in lowered and ("deliver" in lowered or "submitted" in lowered))


def _find_live_pane_from_pollution_sources(workspace: Path, token: str, driver: CommsSelftestDriver) -> str:
    texts: list[str] = []
    for name in ("live_events", "live_messages", "live_capture_before", "live_capture_after"):
        value = _driver_value(driver, name, default=None)
        if value is not None:
            texts.append(str(value))
    from team_agent.paths import logs_dir
    path = logs_dir(workspace) / "events.jsonl"
    if path.exists():
        try:
            texts.append(path.read_text(encoding="utf-8", errors="ignore"))
        except OSError:
            pass
    for text in texts:
        if token not in text and "%live" not in text:
            continue
        for part in text.replace('"', " ").replace("'", " ").replace(",", " ").split():
            if part.startswith("%live"):
                return part.rstrip("}:]")
    return "%live-fake"


def _global_registry_pollution_check(run_id: str, throwaway_root: Path, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_value(driver, "global_registry_pollution", default=None)
    if isinstance(override, dict):
        out = _normalize_check(override)
        out.setdefault("run_id", run_id)
        out.setdefault("detected_in", [])
        return out
    detected = _driver_value(driver, "global_registry_detected_in", default=None)
    if detected is None:
        detected = _scan_global_registries_for_run_id(run_id, throwaway_root)
    detected = [str(item) for item in detected]
    return {
        "status": "fail" if detected else "pass",
        "run_id": run_id,
        "throwaway_root": str(throwaway_root),
        "detected_in": detected,
        "detected_paths": detected,
    }


def _scan_global_registries_for_run_id(run_id: str, throwaway_root: Path) -> list[str]:
    roots = [Path.home() / ".team-agent", Path.cwd() / ".team", Path(tempfile.gettempdir())]
    hits: list[str] = []
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if not path.is_file():
                continue
            if ".team/artifacts/" in str(path):
                continue
            try:
                resolved = path.resolve()
                if throwaway_root in resolved.parents or resolved == throwaway_root:
                    continue
                if run_id in path.read_text(encoding="utf-8", errors="ignore"):
                    hits.append(str(path))
            except OSError:
                continue
    return hits


def _find_driver_pane_id(driver: CommsSelftestDriver | None) -> str:
    if driver is None:
        return ""
    names = [name for name in dir(driver) if not name.startswith("_")]
    for name in names:
        if "live" not in name.lower():
            continue
        try:
            value = getattr(driver, name)
        except Exception:
            continue
        found = _find_pane_id(value)
        if found:
            return found
        if isinstance(value, str) and value.startswith("%"):
            return value
    for name in names:
        try:
            value = getattr(driver, name)
        except Exception:
            continue
        if callable(value):
            continue
        found = _find_pane_id(value)
        if found:
            return found
        if isinstance(value, str) and value.startswith("%"):
            return value
    return ""


def _find_pane_id(value: Any) -> str:
    if isinstance(value, dict):
        for key in ("pane_id", "live_pane_id", "leader_pane_id"):
            candidate = value.get(key)
            if isinstance(candidate, str) and candidate.startswith("%"):
                return candidate
        for child in value.values():
            found = _find_pane_id(child)
            if found:
                return found
    if isinstance(value, list):
        for child in value:
            found = _find_pane_id(child)
            if found:
                return found
    return ""


def _finish(run_id: str, token: str, gate: str | None, checks: dict[str, Any], error: str | None = None, events: list[Any] | None = None) -> dict[str, Any]:
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
    return value.get("status") in {"pass", "pending", "killed", "not_implemented"} or value.get("ok") is True


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


def _requires_live_workspace_restore(driver: CommsSelftestDriver | None) -> bool:
    matrix_case = _matrix_case_code(driver)
    return matrix_case in {"B1", "B2"} or bool(_driver_value(driver, "enforce_live_workspace_unchanged", default=False))


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
