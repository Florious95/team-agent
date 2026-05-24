from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any

from team_agent.diagnose.checks import (
    compact_model_checks,
    model_checks_for_agents,
    profile_checks_for_agents,
    profile_smoke_checks_for_agents,
)
from team_agent.events import EventLog
from team_agent.paths import logs_dir, team_workspace
from team_agent.profiles import compact_profile_check
from team_agent.rust_core import core_binary
from team_agent.simple_yaml import dumps


def preflight(team_dir: Path) -> dict[str, Any]:
    from team_agent.compiler import compile_team
    from team_agent.profiles import profile_dir
    from team_agent.runtime import (
        GHOSTTY_DISPLAY_BACKENDS,
        _attach_team_profile_dirs,
        _ghostty_command,
        ensure_workspace_dirs,
        shutil_which,
    )

    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    ensure_workspace_dirs(workspace)
    ensure_profiles_for_roles(team_dir)
    event_log = EventLog(workspace)
    checks: list[dict[str, Any]] = []
    ok = True
    spec = None
    try:
        compiled = compile_team(team_dir)
        spec = compiled["spec"]
        _attach_team_profile_dirs(spec, team_dir / "team.spec.yaml", workspace, team_dir)
        checks.append({"name": "compile", "ok": True, "agents": [a["id"] for a in spec.get("agents", [])]})
    except Exception as exc:
        ok = False
        checks.append({"name": "compile", "ok": False, "error": str(exc)})
    tmux_path = shutil_which("tmux")
    checks.append({"name": "tmux", "ok": bool(tmux_path), "path": tmux_path})
    ok = ok and bool(tmux_path)
    ghostty = _ghostty_command()
    ghostty_check = {"name": "ghostty", "ok": bool(ghostty), "path": ghostty, "required": False}
    if spec and spec.get("runtime", {}).get("display_backend") in GHOSTTY_DISPLAY_BACKENDS:
        ghostty_check["required"] = True
        ok = ok and bool(ghostty)
    checks.append(ghostty_check)
    if spec:
        profile_checks = profile_checks_for_agents(workspace, spec.get("agents", []))
        profile_failures = [item for item in profile_checks if item.get("ok") is False]
        checks.append({"name": "profiles", "ok": not profile_failures, "checks": [compact_profile_check(item) for item in profile_checks]})
        ok = ok and not profile_failures
        smoke_checks = profile_smoke_checks_for_agents(workspace, spec.get("agents", []))
        smoke_failures = [item for item in smoke_checks if item.get("ok") is False]
        checks.append({"name": "profile_smoke", "ok": not smoke_failures, "checks": [compact_profile_check(item) for item in smoke_checks]})
        ok = ok and not smoke_failures
        model_checks = model_checks_for_agents(spec.get("agents", []), workspace)
        model_failures = [item for item in model_checks if item.get("ok") is False]
        checks.append({"name": "models", "ok": not model_failures, "checks": compact_model_checks(model_checks)})
        ok = ok and not model_failures
    core = core_binary()
    checks.append(
        {
            "name": "rust_core",
            "ok": True,
            "required": False,
            "available": bool(core),
            "path": str(core) if core else None,
            "status": "available" if core else "python_fallback",
        }
    )
    checks.append({"name": "profile_dir", "ok": profile_dir(workspace).exists() or (team_dir / "profiles").exists()})
    details_log = logs_dir(workspace) / f"preflight-{int(time.time())}.json"
    details = {"team_dir": str(team_dir), "checks": checks}
    details_log.write_text(json.dumps(details, indent=2, ensure_ascii=False), encoding="utf-8")
    event_log.write("preflight.complete", ok=ok, details_log=str(details_log), checks=checks)
    blockers = [] if ok else preflight_blockers(checks)
    return {
        "ok": ok,
        "summary": "preflight passed" if ok else "preflight found blockers: " + "; ".join(blockers[:3]),
        "next_actions": [f"team-agent start --team {team_dir} --yes --json"] if ok else preflight_next_actions(blockers),
        "details_log": str(details_log),
        "checks": checks,
        "blockers": blockers,
    }


def start(team_dir: Path, yes: bool = False) -> dict[str, Any]:
    from team_agent.compiler import compile_team
    from team_agent.runtime import launch

    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    spec_path = team_dir / "team.spec.yaml"
    compiled = compile_team(team_dir, spec_path)
    if compiled["spec"].get("context", {}).get("state_file") == "team_state.md":
        state_file = str(team_dir.relative_to(workspace) / "team_state.md") if team_dir.is_relative_to(workspace) else "team_state.md"
        compiled["spec"]["context"]["state_file"] = state_file
        spec_path.write_text(dumps(compiled["spec"]), encoding="utf-8")
    launched = launch(spec_path, auto_approve=yes)
    details_log = logs_dir(workspace) / f"start-{int(time.time())}.json"
    details_log.write_text(json.dumps({"compile": compiled, "launch": launched}, indent=2, ensure_ascii=False), encoding="utf-8")
    return {
        "ok": bool(launched.get("ok")),
        "summary": f"compiled {team_dir} and launched {len(launched.get('agents', []))} agents",
        "next_actions": ["team-agent wait-ready --workspace . --timeout 120 --json"],
        "details_log": str(details_log),
        "spec": str(spec_path),
        "launch": launched,
    }


def preflight_blockers(checks: list[dict[str, Any]]) -> list[str]:
    blockers: list[str] = []
    for check in checks:
        if check.get("ok", True):
            continue
        name = check.get("name") or "check"
        if name == "compile":
            blockers.append(f"compile: {check.get('error')}")
            continue
        for item in check.get("checks", []) or []:
            agent = item.get("agent_id") or item.get("profile") or "-"
            reason = item.get("reason") or item.get("status") or "failed"
            detail = f"{name}: {agent} {reason}"
            if item.get("endpoint"):
                detail += f" endpoint={item['endpoint']}"
            if item.get("proxy_configured"):
                detail += f" proxy={item.get('proxy_url') or item.get('proxy_scheme')}"
            if item.get("proxy_source"):
                detail += f" proxy_source={item['proxy_source']}"
            if item.get("proxy_mode"):
                detail += f" proxy_mode={item['proxy_mode']}"
            if item.get("missing_required"):
                detail += " missing=" + ",".join(item["missing_required"])
            if item.get("effective_model"):
                detail += f" model={item['effective_model']}"
            if item.get("suggestion"):
                detail += f" suggestion={item['suggestion']}"
            blockers.append(detail)
        if not check.get("checks"):
            blockers.append(f"{name}: failed")
    return blockers or ["unknown preflight blocker"]


def preflight_next_actions(blockers: list[str]) -> list[str]:
    actions = ["Fix failed checks, then rerun preflight."]
    if any("proxy_connectivity_failed" in item for item in blockers):
        actions.insert(0, "Allow the profile BASE_URL through the configured proxy, or disable the proxy for Team Agent startup.")
    if any("proxy_source=ambient" in item for item in blockers):
        actions.insert(0, "Current environment proxy is being used for this compatible_api worker; either fix that proxy for BASE_URL, set HTTPS_PROXY/HTTP_PROXY in the profile, or set PROXY_MODE=direct in the profile to bypass proxy for this worker.")
    if any("missing=" in item or "profile_required_values_missing" in item for item in blockers):
        actions.insert(
            0,
            "Ask the human user to fill the local profile file; agents must inspect only with `team-agent profile show <name> --workspace . --json` or the returned --team variant and must not read .team/*/profiles/*.env.",
        )
    if any("model_mismatch" in item or "does not match profile MODEL" in item for item in blockers):
        actions.insert(0, "Keep the model in the profile MODEL field or make the role model exactly match it.")
    return actions


def ensure_profiles_for_roles(team_dir: Path) -> None:
    from team_agent.compiler import _read_front_matter
    from team_agent.profiles import ensure_profile_secret_boundary, ensure_profile_secret_boundary_dir, init_profile

    workspace = team_workspace(team_dir)
    profiles_dir = team_dir / "profiles"
    profiles_dir.mkdir(parents=True, exist_ok=True)
    ensure_profile_secret_boundary(workspace)
    ensure_profile_secret_boundary_dir(profiles_dir)
    for role_doc in sorted((team_dir / "agents").glob("*.md")):
        meta, _ = _read_front_matter(role_doc)
        profile = meta.get("profile")
        auth_mode = meta.get("auth_mode") or "subscription"
        if not profile:
            continue
        if not (profiles_dir / f"{profile}.env").exists() and not (profiles_dir / f"{profile}.example.env").exists():
            init_profile(workspace, str(profile), str(auth_mode))
            if auth_mode == "subscription":
                body = f"AUTH_MODE=subscription\nPROFILE_NAME={profile}\n"
            elif auth_mode == "official_api":
                body = f"AUTH_MODE=official_api\nPROFILE_NAME={profile}\nAPI_KEY=\nMODEL=\n"
            else:
                body = f"AUTH_MODE={auth_mode}\nPROFILE_NAME={profile}\nBASE_URL=\nAPI_KEY=\nMODEL=\n"
            (profiles_dir / f"{profile}.example.env").write_text(body, encoding="utf-8")
