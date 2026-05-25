from __future__ import annotations

from team_agent.message_store.agent_health import delete_agent_health, gc_agent_health, upsert_agent_health
from team_agent.message_store.core import MessageStore
from team_agent.message_store.result_watchers import create_result_watcher, mark_result_watcher
from team_agent.message_store.schema import SCHEMA_VERSION, initialize_schema, utcnow

_REQUIRED_EXPORTS = (
    "MessageStore",
    "SCHEMA_VERSION",
    "initialize_schema",
    "utcnow",
    "upsert_agent_health",
    "delete_agent_health",
    "gc_agent_health",
    "create_result_watcher",
    "mark_result_watcher",
)
for _name in _REQUIRED_EXPORTS:
    if _name not in globals():
        raise ImportError(f"team_agent.message_store missing export: {_name}")

__all__ = list(_REQUIRED_EXPORTS)
