"""Per-CLI idle/turn-state registry — PURE INFRA DATA (Gap 32 C7).

This module is data only: session-file locations, turn-lifecycle marker
descriptions, and per-CLI error white/black lists. It carries no predicate,
abnormal, or wake logic. Adding a new provider is one entry here plus one
reader module under ``provider_state/``; the neutral layers never change.

The registry is shipped with the runtime as infra data — it is NOT
user-mandatory configuration and is never loaded from a workspace.
"""

from __future__ import annotations

from typing import Any

# Each entry is consumed by the matching provider reader. The neutral
# idle_predicate / abnormal_track / wake modules never read provider names.
_PROVIDER_REGISTRY: dict[str, dict[str, Any]] = {
    "claude": {
        "kind": "claude",
        "reader_module": "team_agent.provider_state.claude",
        "source": "infra",
        "file_location": {
            "root": "~/.claude/projects",
            "layout": "<cwd-slug>/<session_id>.jsonl",
            "format": "transcript-jsonl",
        },
        "event_types": {
            "turn_open": "assistant message.stop_reason == tool_use",
            "turn_complete": "assistant message.stop_reason == end_turn",
            "interrupted": "user text == [Request interrupted by user]",
            "tool_error": "user tool_result is_error == true",
            "api_error": "system subtype == api_error and level == error",
        },
        "metadata_ignore": [
            "stop_hook_summary",
            "turn_duration",
            "last-prompt",
            "ai-title",
            "permission-mode",
            "file-history-snapshot",
            "queue-operation",
        ],
        "error_whitelist": [],
        "error_blacklist": [
            "api_error",
            "rate limit",
            "overloaded",
            "traceback",
            "panic",
        ],
        "error_lists": {
            "whitelist": [],
            "blacklist": ["api_error", "rate limit", "overloaded", "traceback", "panic"],
        },
    },
    "codex": {
        "kind": "codex",
        "reader_module": "team_agent.provider_state.codex",
        "source": "infra",
        "file_location": {
            "root": "~/.codex/sessions",
            "layout": "<YYYY>/<MM>/<DD>/rollout-<stamp>-<session_id>.jsonl",
            "format": "rollout-jsonl",
        },
        "event_types": {
            "turn_open": "event_msg payload.type == task_started",
            "turn_complete": "event_msg payload.type == task_complete",
            "interrupted": "event_msg payload.type == turn_aborted and reason == interrupted",
            "failed": "app-server turn.status == failed",
            "approval": "app-server method endswith requestApproval",
        },
        "metadata_ignore": [
            "token_count",
            "agent_message",
            "context_compacted",
            "mcp_tool_call_end",
            "patch_apply_end",
            "web_search_end",
            "thread_goal_updated",
        ],
        "error_whitelist": [],
        "error_blacklist": [
            "failed",
            "api error",
            "rate limit",
            "overloaded",
            "traceback",
            "panic",
        ],
        "error_lists": {
            "whitelist": [],
            "blacklist": ["failed", "api error", "rate limit", "overloaded", "traceback", "panic"],
        },
    },
}


def get_provider_registry(provider: str | None = None) -> Any:
    """Return the infra registry.

    With no argument, returns a copy of the whole per-CLI registry mapping.
    With a provider name, returns that provider's entry (or ``None``).
    """
    if provider is None:
        return {name: _copy_entry(entry) for name, entry in _PROVIDER_REGISTRY.items()}
    entry = _PROVIDER_REGISTRY.get(provider)
    return _copy_entry(entry) if entry is not None else None


def supported_providers() -> list[str]:
    return sorted(_PROVIDER_REGISTRY)


def _copy_entry(entry: dict[str, Any]) -> dict[str, Any]:
    import copy

    return copy.deepcopy(entry)
