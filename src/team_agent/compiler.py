from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

from team_agent.errors import ValidationError
from team_agent.paths import team_workspace
from team_agent.profiles import AUTH_MODES, known_profiles, load_profile
from team_agent.rust_core import contains_inline_secret, validate_profile_metadata
from team_agent.simple_yaml import dumps, loads
from team_agent.spec import SUPPORTED_PROVIDERS, validate_spec


REQUIRED_ROLE_FIELDS = {"name", "role", "provider", "tools"}
DEFAULT_PROVIDER_MODELS = {
    "codex": "gpt-5.5",
    "claude": "claude-sonnet-4-6",
    "claude_code": "claude-sonnet-4-6",
}


def compile_team(team_dir: Path, out_path: Path | None = None) -> dict[str, Any]:
    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    team_doc = team_dir / "TEAM.md"
    agents_dir = team_dir / "agents"
    if not team_doc.exists():
        raise ValidationError(f"{team_doc}: missing TEAM.md")
    if not agents_dir.exists():
        raise ValidationError(f"{agents_dir}: missing agents directory")

    profile_names = known_profiles(team_dir)
    team_meta, team_body = _read_front_matter(team_doc)
    default_model = team_meta.get("default_model") or team_meta.get("model")
    provider_models = _provider_model_defaults(team_meta)
    default_auth_mode = team_meta.get("default_auth_mode") or "subscription"
    default_profile = team_meta.get("default_profile")
    agents = []
    routing_rules = []
    startup_order = []
    for role_doc in sorted(agents_dir.glob("*.md")):
        meta, body = _role_doc_meta_for_team(
            role_doc,
            team_meta,
            workspace,
            team_dir,
            profile_names,
            default_auth_mode,
            default_profile,
            provider_models,
            default_model,
        )
        agent_id = str(meta["name"])
        agent = _agent_from_role_doc(meta, body, workspace, agent_id)
        agents.append(agent)
        routing_rules.append({"id": f"route-{agent_id}", "match": {"assignee": [agent_id]}, "assign_to": agent_id, "priority": 10})
        startup_order.append(agent_id)
    if not agents:
        raise ValidationError(f"{agents_dir}: no role docs found")

    default_agent = agents[0]["id"]
    team_name = str(team_meta.get("name") or team_dir.parent.name or "team-agent-team")
    spec = {
        "version": 1,
        "team": {
            "name": team_name,
            "mode": "supervisor_worker",
            "objective": str(team_meta.get("objective") or team_body.strip() or "Team Agent document-driven team."),
            "workspace": str(workspace),
        },
        "leader": {
            "id": "leader",
            "role": str(team_meta.get("leader_role") or "leader"),
            "provider": str(team_meta.get("provider") or "codex"),
            "model": team_meta.get("model"),
            "tools": ["fs_read", "fs_list", "mcp_team"],
            "context_policy": {
                "keep_user_thread": True,
                "receive_worker_outputs": "business_messages_and_short_summaries",
                "max_worker_result_tokens": 2000,
            },
        },
        "agents": agents,
        "routing": {"default_assignee": default_agent, "rules": routing_rules},
        "communication": {
            "protocol": "mcp_inbox",
            "topology": "leader_centered",
            "worker_to_worker": bool(team_meta.get("worker_to_worker", True)),
            "ack_timeout_sec": 60,
            "result_format": "result_envelope_v1",
            "message_store": {"sqlite": ".team/runtime/team.db", "mirror_files": ".team/messages"},
        },
        "runtime": {
            "backend": "tmux",
            "display_backend": str(team_meta.get("display_backend") or "adaptive"),
            "session_name": str(team_meta.get("session_name") or f"team-{_slug(team_name)}"),
            "auto_launch": True,
            "require_user_approval_before_launch": True,
            "max_active_agents": min(len(agents), 2),
            "startup_order": startup_order,
            "dangerous_auto_approve": bool(team_meta.get("dangerous_auto_approve", False)),
            "fast": bool(team_meta.get("fast", False)),
            "tick_interval_sec": int(team_meta.get("tick_interval_sec", 2)),
            "push_min_interval_sec": int(team_meta.get("push_min_interval_sec", 60)),
            "stuck_timeout_sec": int(team_meta.get("stuck_timeout_sec", 300)),
        },
        "context": {
            "state_file": "team_state.md",
            "artifact_dir": ".team/artifacts",
            "log_dir": ".team/logs",
            "summarization": {
                "worker_full_logs": "retain_outside_leader_context",
                "state_update": "after_each_result",
            },
        },
        "tasks": [
            {
                "id": "task_initial",
                "title": "Initial document-driven team task",
                "type": "implementation",
                "assignee": default_agent,
                "deps": [],
                "acceptance": ["Worker reports valid result_envelope_v1"],
                "status": "pending",
                "requires_tools": ["mcp_team"],
                "files": [],
                "risk": "low",
            }
        ],
    }
    validate_spec(spec, base_dir=workspace)
    if out_path:
        out_path.write_text(dumps(spec), encoding="utf-8")
    return {"ok": True, "team_dir": str(team_dir), "out": str(out_path) if out_path else None, "spec": spec}


def compile_role_doc_agent(role_doc: Path, team_dir: Path, agent_id: str | None = None) -> dict[str, Any]:
    meta, body = _read_front_matter(role_doc.resolve())
    return compile_role_entry_agent(role_doc.resolve(), team_dir, meta, body, agent_id)


def compile_role_entry_agent(
    role_doc: Path,
    team_dir: Path,
    meta: dict[str, Any],
    body: str,
    agent_id: str | None = None,
) -> dict[str, Any]:
    team_dir = team_dir.resolve()
    workspace = team_workspace(team_dir)
    team_doc = team_dir / "TEAM.md"
    if not team_doc.exists():
        raise ValidationError(f"{team_doc}: missing TEAM.md")
    profile_names = known_profiles(team_dir)
    team_meta, _team_body = _read_front_matter(team_doc)
    meta, body = _role_doc_meta_for_team(
        role_doc,
        team_meta,
        workspace,
        team_dir,
        profile_names,
        team_meta.get("default_auth_mode") or "subscription",
        team_meta.get("default_profile"),
        _provider_model_defaults(team_meta),
        team_meta.get("default_model") or team_meta.get("model"),
        role_meta=meta,
        role_body=body,
    )
    return _agent_from_role_doc(meta, body, workspace, str(agent_id or meta["name"]))


def _read_front_matter(path: Path) -> tuple[dict[str, Any], str]:
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---\n"):
        return {}, text
    end = text.find("\n---", 4)
    if end == -1:
        raise ValidationError(f"{path}: unterminated front matter")
    raw = text[4:end]
    body = text[end + 4 :].lstrip("\n")
    data = loads(raw) if raw.strip() else {}
    if not isinstance(data, dict):
        raise ValidationError(f"{path}: front matter must be a YAML object")
    return data, body


def _role_doc_meta_for_team(
    role_doc: Path,
    team_meta: dict[str, Any],
    workspace: Path,
    team_dir: Path,
    profile_names: set[str],
    default_auth_mode: Any,
    default_profile: Any,
    provider_models: dict[str, str],
    default_model: Any,
    role_meta: dict[str, Any] | None = None,
    role_body: str | None = None,
) -> tuple[dict[str, Any], str]:
    meta, body = (role_meta, role_body) if role_meta is not None and role_body is not None else _read_front_matter(role_doc)
    meta = copy.deepcopy(meta)
    if "auth_mode" not in meta and default_auth_mode is not None:
        meta["auth_mode"] = default_auth_mode
    if "profile" not in meta and default_profile is not None:
        meta["profile"] = default_profile
    profile_model = _profile_model(workspace, meta.get("profile"), team_dir / "profiles")
    if "model" not in meta and not (meta.get("auth_mode") == "compatible_api" and profile_model):
        meta["model"] = _default_model_for_provider(meta.get("provider"), provider_models, default_model)
    _validate_role_doc(role_doc, meta, body, profile_names, profile_model)
    return meta, body


def _agent_from_role_doc(meta: dict[str, Any], body: str, workspace: Path, agent_id: str) -> dict[str, Any]:
    agent = {
        "id": agent_id,
        "role": str(meta["role"]),
        "provider": str(meta["provider"]),
        "model": str(meta["model"]) if meta.get("model") is not None else None,
        "auth_mode": str(meta["auth_mode"]),
        "working_directory": str(workspace),
        "system_prompt": {"inline": body.strip() or str(meta["role"]), "file": None},
        "tools": _normalize_tools(list(meta["tools"] or [])),
        "permission_mode": "restricted",
        "preferred_for": [agent_id, str(meta["role"])],
        "avoid_for": [],
        "output_contract": {
            "format": "result_envelope_v1",
            "required_fields": ["task_id", "status", "summary", "artifacts"],
        },
    }
    if meta.get("profile"):
        agent["profile"] = str(meta["profile"])
        agent["credential_ref"] = f"profile:{meta['profile']}"
    return agent


def _validate_role_doc(
    path: Path,
    meta: dict[str, Any],
    body: str,
    profile_names: set[str],
    profile_model: str | None = None,
) -> None:
    errors = []
    missing = sorted(REQUIRED_ROLE_FIELDS - set(meta))
    for field in missing:
        errors.append(f"{path}: missing front matter field {field}")
    provider = meta.get("provider")
    if provider not in SUPPORTED_PROVIDERS:
        errors.append(f"{path}: unknown provider {provider!r}")
    auth_mode = meta.get("auth_mode")
    if auth_mode not in AUTH_MODES:
        errors.append(f"{path}: unknown auth_mode {auth_mode!r}")
    profile = meta.get("profile")
    if profile and profile not in profile_names:
        errors.append(f"{path}: unknown profile {profile!r}")
    if auth_mode != "subscription" and not profile:
        errors.append(f"{path}: profile is required when auth_mode is {auth_mode!r}")
    role_model = str(meta.get("model") or "") or None
    if auth_mode == "compatible_api" and role_model and profile_model and role_model != profile_model:
        errors.append(
            f"{path}: role model {role_model!r} does not match profile MODEL {profile_model!r}; "
            "keep the model in one place or make both values identical"
        )
    if not isinstance(meta.get("tools"), list):
        errors.append(f"{path}: tools must be a list")
    if profile:
        profile_check = validate_profile_metadata(
            {
                "provider": provider or "",
                "model": role_model or profile_model or "",
                "auth_mode": auth_mode or "",
                "profile": profile or "",
                "credential_ref": f"profile:{profile}",
            }
        )
        if not profile_check.get("ok"):
            errors.extend(f"{path}: {err}" for err in profile_check.get("errors", []))
    for value in [body, dumps(meta)]:
        if contains_inline_secret(value):
            errors.append(f"{path}: probable inline secret detected; use profile/credential_ref instead")
            break
    if errors:
        raise ValidationError("\n".join(errors))


def _normalize_tools(tools: list[Any]) -> list[str]:
    mapping = {"shell": "execute_bash"}
    return [mapping.get(str(tool), str(tool)) for tool in tools]


def _profile_model(workspace: Path, profile: Any, profiles_dir: Path | None = None) -> str | None:
    if not profile:
        return None
    values = load_profile(workspace, str(profile), profiles_dir).get("values", {})
    if not isinstance(values, dict):
        return None
    return values.get("MODEL") or values.get("ANTHROPIC_MODEL")


def _provider_model_defaults(team_meta: dict[str, Any]) -> dict[str, str]:
    raw = team_meta.get("provider_models") or {}
    if not isinstance(raw, dict):
        return {}
    return {str(provider): str(model) for provider, model in raw.items() if model}


def _default_model_for_provider(
    provider: Any,
    provider_models: dict[str, str],
    default_model: Any,
) -> str | None:
    provider_id = str(provider or "")
    if provider_id in provider_models:
        return provider_models[provider_id]
    if provider_id == "claude_code" and "claude" in provider_models:
        return provider_models["claude"]
    if provider_id == "claude" and "claude_code" in provider_models:
        return provider_models["claude_code"]
    if default_model is not None:
        return str(default_model)
    return DEFAULT_PROVIDER_MODELS.get(provider_id)


def _slug(value: str) -> str:
    out = []
    for ch in value:
        if ch.isalnum() or ch in {"-", "_"}:
            out.append(ch)
        else:
            out.append("-")
    slug = "".join(out).strip("-_")
    return slug or "team"
