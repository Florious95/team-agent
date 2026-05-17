from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.errors import ValidationError
from team_agent.permissions import CANONICAL_TOOLS, expand_tools
from team_agent.profiles import AUTH_MODES
from team_agent.simple_yaml import loads
from team_agent.task_graph import find_dependency_cycle

SUPPORTED_PROVIDERS = {"claude", "claude_code", "codex", "gemini_cli", "fake"}


def load_yaml(path: Path) -> dict[str, Any]:
    try:
        data = loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise ValidationError(f"Cannot read {path}: {exc}") from exc
    except ValueError as exc:
        raise ValidationError(f"Invalid YAML in {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise ValidationError(f"{path} must contain a YAML object")
    return data


def load_spec(path: Path) -> dict[str, Any]:
    spec = load_yaml(path)
    validate_spec(spec, base_dir=path.parent)
    return spec


def validate_spec(spec: dict[str, Any], base_dir: Path | None = None) -> None:
    messages = _basic_schema_errors(spec)
    messages.extend(_semantic_errors(spec, base_dir or Path.cwd()))
    if messages:
        joined = "\n".join(f"- {m}" for m in messages)
        raise ValidationError(f"team.spec.yaml validation failed:\n{joined}")


RESULT_COLLECTION_SCHEMAS: dict[str, tuple[set[str], set[str]]] = {
    "changes": ({"path", "kind", "description"}, {"path", "kind", "description"}),
    "tests": ({"command", "status"}, {"command", "status", "detail"}),
    "risks": ({"severity", "description"}, {"severity", "description"}),
    "artifacts": ({"path", "description"}, {"path", "description"}),
    "next_actions": ({"description"}, {"description"}),
}


def validate_result_envelope(envelope: dict[str, Any]) -> None:
    errors = _result_schema_errors(envelope)
    if errors:
        joined = "\n".join(f"- {error}" for error in errors)
        raise ValidationError(f"result_envelope_v1 validation failed:\n{joined}")


def _basic_schema_errors(spec: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    root_keys = {"version", "team", "leader", "agents", "routing", "communication", "runtime", "context", "tasks"}
    _check_keys(spec, "/", root_keys, root_keys, errors)
    if spec.get("version") != 1:
        errors.append("/version: must equal 1")
    _check_keys(spec.get("team"), "/team", {"name", "mode", "objective", "workspace"}, {"name", "mode", "objective", "workspace"}, errors)
    if spec.get("team", {}).get("mode") not in {"supervisor_worker", "swarm_limited"}:
        errors.append("/team/mode: invalid mode")
    _check_keys(
        spec.get("leader"),
        "/leader",
        {"id", "role", "provider", "model", "tools", "context_policy"},
        {"id", "role", "provider", "model", "tools", "context_policy"},
        errors,
    )
    _check_context_policy(spec.get("leader", {}).get("context_policy"), errors)
    if not isinstance(spec.get("agents"), list) or not spec.get("agents"):
        errors.append("/agents: must be a non-empty list")
    else:
        for idx, agent in enumerate(spec["agents"]):
            _check_agent(agent, f"/agents/{idx}", errors)
    _check_routing(spec.get("routing"), errors)
    _check_communication(spec.get("communication"), errors)
    _check_runtime(spec.get("runtime"), errors)
    _check_context(spec.get("context"), errors)
    if not isinstance(spec.get("tasks"), list):
        errors.append("/tasks: must be a list")
    else:
        for idx, task in enumerate(spec["tasks"]):
            _check_task(task, f"/tasks/{idx}", errors)
    return errors


def _result_schema_errors(envelope: Any) -> list[str]:
    errors: list[str] = []
    required = {"schema_version", "task_id", "agent_id", "status", "summary", "changes", "tests", "risks", "artifacts", "next_actions"}
    _check_keys(envelope, "/", required, required, errors)
    if not isinstance(envelope, dict):
        return errors
    if envelope.get("schema_version") != "result_envelope_v1":
        errors.append("/schema_version: must be result_envelope_v1")
    for field in ["task_id", "agent_id", "summary"]:
        if field in envelope and not isinstance(envelope[field], str):
            errors.append(f"/{field}: must be a string")
        elif field in envelope and not envelope[field]:
            errors.append(f"/{field}: must not be empty")
    if envelope.get("status") not in {"success", "blocked", "failed", "partial"}:
        errors.append("/status: invalid result status")
    if "schema" in envelope:
        errors.append("/schema: use schema_version, not schema")
    for field, (item_required, item_allowed) in RESULT_COLLECTION_SCHEMAS.items():
        if field not in envelope:
            continue
        value = envelope[field]
        if not isinstance(value, list):
            errors.append(f"/{field}: must be a list")
            continue
        for idx, item in enumerate(value):
            item_path = f"/{field}/{idx}"
            _check_keys(item, item_path, item_required, item_allowed, errors)
            if not isinstance(item, dict):
                continue
            if field == "changes" and item.get("kind") not in {"created", "modified", "deleted", "observed"}:
                errors.append(f"{item_path}/kind: invalid change kind")
            if field == "tests" and item.get("status") not in {"passed", "failed", "not_run", "skipped"}:
                errors.append(f"{item_path}/status: invalid test status")
            if field == "risks" and item.get("severity") not in {"low", "medium", "high"}:
                errors.append(f"{item_path}/severity: invalid risk severity")
            for key, child in item.items():
                if key in item_allowed and not isinstance(child, str):
                    errors.append(f"{item_path}/{key}: must be a string")
    return errors


def _check_agent(agent: Any, path: str, errors: list[str]) -> None:
    required = {"id", "role", "provider", "model", "working_directory", "system_prompt", "tools", "permission_mode", "preferred_for", "avoid_for", "output_contract"}
    allowed = required | {"paused", "auth_mode", "profile", "credential_ref"}
    _check_keys(agent, path, required, allowed, errors)
    if not isinstance(agent, dict):
        return
    _check_keys(agent.get("system_prompt"), f"{path}/system_prompt", {"inline", "file"}, {"inline", "file"}, errors)
    _check_list(agent.get("tools"), f"{path}/tools", errors)
    _check_list(agent.get("preferred_for"), f"{path}/preferred_for", errors)
    _check_list(agent.get("avoid_for"), f"{path}/avoid_for", errors)
    _check_keys(agent.get("output_contract"), f"{path}/output_contract", {"format", "required_fields"}, {"format", "required_fields"}, errors)
    if agent.get("output_contract", {}).get("format") != "result_envelope_v1":
        errors.append(f"{path}/output_contract/format: must be result_envelope_v1")


def _check_context_policy(policy: Any, errors: list[str]) -> None:
    _check_keys(
        policy,
        "/leader/context_policy",
        {"keep_user_thread", "receive_worker_outputs", "max_worker_result_tokens"},
        {"keep_user_thread", "receive_worker_outputs", "max_worker_result_tokens"},
        errors,
    )


def _check_routing(routing: Any, errors: list[str]) -> None:
    _check_keys(routing, "/routing", {"default_assignee", "rules"}, {"default_assignee", "rules"}, errors)
    if not isinstance(routing, dict):
        return
    if not isinstance(routing.get("rules"), list):
        errors.append("/routing/rules: must be a list")
        return
    for idx, rule in enumerate(routing["rules"]):
        allowed = {"id", "when", "match", "assign_to", "priority"}
        required = {"id", "assign_to", "priority"}
        _check_keys(rule, f"/routing/rules/{idx}", required, allowed, errors)
        if isinstance(rule, dict) and not (rule.get("when") or rule.get("match")):
            errors.append(f"/routing/rules/{idx}: must include when or match")


def _check_communication(comm: Any, errors: list[str]) -> None:
    required = {"protocol", "topology", "worker_to_worker", "ack_timeout_sec", "result_format", "message_store"}
    _check_keys(comm, "/communication", required, required, errors)
    if not isinstance(comm, dict):
        return
    if comm.get("protocol") not in {"mcp_inbox", "file_bus"}:
        errors.append("/communication/protocol: invalid protocol")
    if comm.get("result_format") != "result_envelope_v1":
        errors.append("/communication/result_format: must be result_envelope_v1")
    _check_keys(comm.get("message_store"), "/communication/message_store", {"sqlite", "mirror_files"}, {"sqlite", "mirror_files"}, errors)


def _check_runtime(runtime: Any, errors: list[str]) -> None:
    required = {"backend", "display_backend", "session_name", "auto_launch", "require_user_approval_before_launch", "max_active_agents", "startup_order"}
    allowed = required | {
        "dangerous_auto_approve",
        "auto_attach_leader",
        "fast",
        "tick_interval_sec",
        "push_min_interval_sec",
        "stuck_timeout_sec",
    }
    _check_keys(runtime, "/runtime", required, allowed, errors)
    if not isinstance(runtime, dict):
        return
    if runtime.get("backend") not in {"tmux", "pty"}:
        errors.append("/runtime/backend: invalid backend")
    if runtime.get("display_backend") not in {"none", "tmux_attach", "iterm", "ghostty", "ghostty_window"}:
        errors.append("/runtime/display_backend: invalid display backend")
    if "dangerous_auto_approve" in runtime and not isinstance(runtime["dangerous_auto_approve"], bool):
        errors.append("/runtime/dangerous_auto_approve: must be a boolean")
    _check_list(runtime.get("startup_order"), "/runtime/startup_order", errors)


def _check_context(context: Any, errors: list[str]) -> None:
    required = {"state_file", "artifact_dir", "log_dir", "summarization"}
    _check_keys(context, "/context", required, required, errors)
    if isinstance(context, dict):
        _check_keys(context.get("summarization"), "/context/summarization", {"worker_full_logs", "state_update"}, {"worker_full_logs", "state_update"}, errors)


def _check_task(task: Any, path: str, errors: list[str]) -> None:
    required = {"id", "title", "type", "assignee", "deps", "acceptance", "status"}
    allowed = required | {"description", "requires_tools", "files", "risk", "retry_limit", "human_confirmation"}
    _check_keys(task, path, required, allowed, errors)
    if not isinstance(task, dict):
        return
    _check_list(task.get("deps"), f"{path}/deps", errors)
    _check_list(task.get("acceptance"), f"{path}/acceptance", errors)
    if task.get("status") not in {"pending", "ready", "running", "blocked", "needs_retry", "done", "failed", "cancelled"}:
        errors.append(f"{path}/status: invalid task status")


def _check_keys(obj: Any, path: str, required: set[str], allowed: set[str], errors: list[str]) -> None:
    if not isinstance(obj, dict):
        errors.append(f"{path}: must be an object")
        return
    missing = sorted(required - set(obj))
    for key in missing:
        errors.append(f"{path.rstrip('/')}/{key}: missing required field")
    unknown = sorted(set(obj) - allowed)
    for key in unknown:
        errors.append(f"{path.rstrip('/')}/{key}: unknown field")


def _check_list(value: Any, path: str, errors: list[str]) -> None:
    if not isinstance(value, list):
        errors.append(f"{path}: must be a list")


def _semantic_errors(spec: dict[str, Any], base_dir: Path) -> list[str]:
    errors: list[str] = []
    leader = spec.get("leader", {})
    agents = spec.get("agents", [])
    agent_ids = {a.get("id") for a in agents if isinstance(a, dict)}
    all_ids = set(agent_ids)
    if leader.get("id"):
        all_ids.add(leader["id"])

    for path, provider in [("/leader/provider", leader.get("provider"))]:
        if provider not in SUPPORTED_PROVIDERS:
            errors.append(f"{path}: unknown provider {provider!r}")
    for idx, agent in enumerate(agents):
        provider = agent.get("provider")
        if provider not in SUPPORTED_PROVIDERS:
            errors.append(f"/agents/{idx}/provider: unknown provider {provider!r}")
        auth_mode = agent.get("auth_mode")
        if auth_mode is not None and auth_mode not in AUTH_MODES:
            errors.append(f"/agents/{idx}/auth_mode: unknown auth_mode {auth_mode!r}")
        prompt_file = agent.get("system_prompt", {}).get("file")
        if prompt_file:
            candidate = Path(prompt_file)
            if not candidate.is_absolute():
                candidate = base_dir / candidate
            if not candidate.exists():
                errors.append(f"/agents/{idx}/system_prompt/file: file not found: {candidate}")
        for tool in expand_tools(agent.get("tools", [])):
            if tool not in CANONICAL_TOOLS:
                errors.append(f"/agents/{idx}/tools: unknown tool {tool!r}")

    leader_tools = leader.get("tools", [])
    for tool in expand_tools(leader_tools):
        if tool not in CANONICAL_TOOLS:
            errors.append(f"/leader/tools: unknown tool {tool!r}")

    routing = spec.get("routing", {})
    default_assignee = routing.get("default_assignee")
    if default_assignee and default_assignee not in all_ids:
        errors.append(f"/routing/default_assignee: unknown agent {default_assignee!r}")
    for idx, rule in enumerate(routing.get("rules", [])):
        target = rule.get("assign_to")
        if target not in all_ids:
            errors.append(f"/routing/rules/{idx}/assign_to: unknown agent {target!r}")

    tasks = spec.get("tasks", [])
    task_ids = {t.get("id") for t in tasks if isinstance(t, dict)}
    for idx, task in enumerate(tasks):
        assignee = task.get("assignee")
        if assignee and assignee not in all_ids:
            errors.append(f"/tasks/{idx}/assignee: unknown agent {assignee!r}")
        for dep in task.get("deps", []):
            if dep not in task_ids:
                errors.append(f"/tasks/{idx}/deps: unknown dependency {dep!r}")

    cycle = find_dependency_cycle(tasks)
    if cycle:
        errors.append(f"/tasks: dependency cycle detected: {' -> '.join(cycle)}")
    return errors


def workspace_from_spec(spec: dict[str, Any], spec_path: Path | None = None) -> Path:
    raw = spec.get("team", {}).get("workspace") or "."
    path = Path(raw)
    if path.is_absolute():
        return path
    base = spec_path.parent if spec_path else Path.cwd()
    return (base / path).resolve()
