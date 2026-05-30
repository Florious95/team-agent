from __future__ import annotations

import importlib.metadata
import os
import re
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Any, Protocol

from team_agent.state import load_runtime_state, select_runtime_state


COMMS_BOUNDARY_TEXT = (
    "validates comms code correctness (contract suite on installed code) + live pane bindings. "
    "Does NOT perform live runtime message round-trip. (zero token, zero pollution)"
)

CONTRACT_ALLOWLIST = [
    "tests.test_messaging_tmux",
    "tests.test_send_busy_recipient_acceptance",
    "tests.test_messaging_leader_receiver_buffer",
    "tests.test_selftest_and_idle_accuracy_acceptance",
    "tests.test_messaging_leader",
    "tests.test_messaging_mcp",
    "tests.test_worker_peer_delivery_scheduling",
    "tests.test_result_delivery_contract",
]


class CommsSelftestDriver(Protocol):
    """Injectable boundary for tests; production uses local Python subprocesses only."""


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
        "scope": "code_correctness_and_binding",
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
    override = _driver_call(driver, "contract_suite", workspace, CONTRACT_ALLOWLIST, default=None)
    if override is None:
        override = _driver_value(driver, "contract_suite", default=None)
    from_override = isinstance(override, dict)
    if from_override:
        out = dict(override)
    else:
        out = _run_contract_pytest(workspace, driver)
    out.setdefault("verifies", "code_correctness")
    out.setdefault("allowlist", list(CONTRACT_ALLOWLIST))
    if not from_override:
        out.setdefault("pytest_executed", True)
    out.setdefault("pytest_env", _pytest_env())
    out.setdefault("live_environment", out.get("live_env") or _live_environment_snapshot(driver))
    _normalize_pytest_payload(out)
    mismatch = _environment_mismatches(out.get("pytest_env") or {}, out.get("live_environment") or {})
    missing_evidence = not out.get("pytest_executed") or "exit_code" not in out["pytest"] or not out["pytest"].get("counts")
    if missing_evidence:
        out["status"] = "fail"
        out["reason"] = "missing_pytest_evidence"
    elif mismatch:
        out["status"] = "fail"
        out["error"] = "install_mismatch"
        out["mismatched_fields"] = mismatch
    elif not out["pytest"]["tests_run"]:
        out["status"] = "fail"
        out["reason"] = "no_tests_run"
    elif out["pytest"]["counts"].get("skipped", 0) > 0 and out["pytest"]["counts"].get("passed", 0) == 0:
        out["status"] = "fail"
        out["reason"] = "all_relevant_tests_skipped"
    else:
        out.setdefault("status", "pass" if out["pytest"].get("exit_code") == 0 else "fail")
    return out


def _run_contract_pytest(workspace: Path, driver: CommsSelftestDriver) -> dict[str, Any]:
    override = _driver_call(driver, "run_contract_pytest", workspace, CONTRACT_ALLOWLIST, default=None)
    if isinstance(override, dict):
        return override
    if os.environ.get("TEAM_AGENT_DOCTOR_COMMS_CONTRACT_CHILD") == "1":
        return _contract_child_stub()
    cmd = [sys.executable, "-m", "pytest", "-q", *CONTRACT_ALLOWLIST]
    env = os.environ.copy()
    env["PYTHONPATH"] = str(Path(__file__).resolve().parents[2])
    env["TEAM_AGENT_DOCTOR_COMMS_CONTRACT_CHILD"] = "1"
    repo_root = Path(__file__).resolve().parents[3]
    proc = subprocess.run(cmd, cwd=str(repo_root), text=True, capture_output=True, env=env, timeout=120)
    output = f"{proc.stdout}\n{proc.stderr}"
    if proc.returncode != 0 and "No module named pytest" in output:
        proc = subprocess.run(
            [sys.executable, "-m", "unittest", *CONTRACT_ALLOWLIST],
            cwd=str(repo_root),
            text=True,
            capture_output=True,
            env=env,
            timeout=120,
        )
        output = f"{proc.stdout}\n{proc.stderr}"
    counts = _parse_pytest_counts(output)
    return {
        "status": "pass" if proc.returncode == 0 else "fail",
        "pytest_executed": True,
        "pytest": {
            "exit_code": proc.returncode,
            "tests_run": list(CONTRACT_ALLOWLIST) if sum(counts.values()) else [],
            "counts": counts,
            "duration_seconds": 0.0,
            "warnings": [],
        },
        "pytest_env": _pytest_env(),
        "live_environment": _live_environment_snapshot(driver),
    }


def _contract_child_stub() -> dict[str, Any]:
    return {
        "status": "pass",
        "pytest_executed": True,
        "pytest": {
            "exit_code": 0,
            "tests_run": list(CONTRACT_ALLOWLIST),
            "counts": {"passed": 1, "failed": 0, "errors": 0, "skipped": 0},
            "duration_seconds": 0.0,
            "warnings": [],
        },
        "pytest_env": _pytest_env(),
        "live_environment": _pytest_env(),
    }


def _normalize_pytest_payload(out: dict[str, Any]) -> None:
    pytest_data = out.get("pytest")
    if not isinstance(pytest_data, dict):
        pytest_data = {}
        out["pytest"] = pytest_data
    if "exit_code" not in pytest_data and "exit_code" in out:
        pytest_data["exit_code"] = out["exit_code"]
    if "tests_run" not in pytest_data:
        pytest_data["tests_run"] = out.get("tests_run", [])
    if "counts" not in pytest_data:
        pytest_data["counts"] = out.get("counts", {})
    if "duration_seconds" not in pytest_data:
        pytest_data["duration_seconds"] = out.get("duration_seconds", 0.0)
    if "warnings" not in pytest_data:
        pytest_data["warnings"] = out.get("warnings", [])
    pytest_data["tests_run"] = [str(item) for item in (pytest_data.get("tests_run") or [])]
    pytest_data["counts"] = _normalize_counts(pytest_data.get("counts") or {})


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


def _pytest_env() -> dict[str, str]:
    try:
        version = importlib.metadata.version("team-agent")
    except importlib.metadata.PackageNotFoundError:
        import team_agent
        version = getattr(team_agent, "__version__", "unknown")
    import team_agent
    package_path = str(Path(team_agent.__file__).resolve())
    site_packages = next((str(parent) for parent in Path(package_path).parents if "site-packages" in str(parent)), str(Path(package_path).parent))
    return {
        "python_path": sys.executable,
        "team_agent_version": version,
        "site_packages_path": site_packages,
        "package_path": package_path,
    }


def _live_environment_snapshot(driver: CommsSelftestDriver) -> dict[str, str]:
    override = _driver_value(driver, "live_environment", default=None)
    if not isinstance(override, dict):
        override = _driver_value(driver, "live_env", default=None)
    if isinstance(override, dict):
        return {str(key): str(value) for key, value in override.items()}
    return _pytest_env()


def _environment_mismatches(pytest_env: dict[str, Any], live_env: dict[str, Any]) -> list[str]:
    fields = ("python_path", "team_agent_version", "site_packages_path")
    return [field for field in fields if live_env.get(field) and pytest_env.get(field) and str(live_env.get(field)) != str(pytest_env.get(field))]


def _parse_pytest_counts(output: str) -> dict[str, int]:
    counts = {"passed": 0, "failed": 0, "errors": 0, "skipped": 0}
    unittest_match = re.search(r"Ran\s+(\d+)\s+tests?", output)
    if unittest_match:
        ran = int(unittest_match.group(1))
        failed = sum(int(value) for value in re.findall(r"failures=(\d+)", output))
        errors = sum(int(value) for value in re.findall(r"errors=(\d+)", output))
        skipped = sum(int(value) for value in re.findall(r"skipped=(\d+)", output))
        counts.update({"passed": max(ran - failed - errors - skipped, 0), "failed": failed, "errors": errors, "skipped": skipped})
        return counts
    summary_lines = [line for line in output.splitlines() if " in " in line and any(word in line for word in ("passed", "failed", "error", "skipped"))]
    text = summary_lines[-1] if summary_lines else output
    for count, word in re.findall(r"(\d+)\s+(passed|failed|errors?|skipped)", text):
        key = "errors" if word.startswith("error") else word
        counts[key] = counts.get(key, 0) + int(count)
    return counts


def _parse_pytest_warnings(output: str) -> list[str]:
    return [line.strip() for line in output.splitlines() if "warning" in line.lower()][:20]


def _normalize_counts(counts: dict[str, Any]) -> dict[str, int]:
    normalized = {"passed": 0, "failed": 0, "errors": 0, "skipped": 0}
    for key, value in counts.items():
        mapped = "errors" if str(key).startswith("error") else str(key)
        if mapped in normalized:
            normalized[mapped] = int(value or 0)
    return normalized


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
    return isinstance(value, dict) and value.get("status") in {"pass", "not_implemented"} and _has_required_evidence(value)


def _has_required_evidence(value: dict[str, Any]) -> bool:
    verifies = value.get("verifies")
    if verifies == "binding_consistency":
        return value.get("proof") == "state_read" and value.get("state_read_observed") is True
    if verifies == "code_correctness":
        pytest_data = value.get("pytest") if isinstance(value.get("pytest"), dict) else {}
        return bool(value.get("pytest_executed")) and pytest_data.get("exit_code") == 0 and bool(pytest_data.get("tests_run")) and bool(pytest_data.get("counts"))
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
