"""Provider turn-state readers behind one shared interface (Gap 32 §6).

``read_turn_state`` is the single entry the rest of the runtime uses; provider
dispatch happens here (and in registry data), so the neutral predicate /
abnormal / wake modules never name a provider.
"""

from __future__ import annotations

import importlib
from typing import Any

from team_agent.provider_state.registry import get_provider_registry

_READER_CACHE: dict[str, Any] = {}


def read_turn_state(
    provider: str,
    session_log_text: str,
    *,
    process: Any = None,
    file_silence_seconds: float = 0,
    registry: Any = None,
) -> dict[str, Any]:
    """Classify a node's turn state from its provider session-log text.

    Returns the stable dict shape: state / turn_id / reason / source /
    annotations / diagnostics. A missing/unknown provider or an unreadable
    file fails safe to ``unknown`` (never idle, Gap 32 C5).
    """
    _ = file_silence_seconds  # open-turn beats silence (C14); silence never forces idle
    reader = _reader_for(provider, registry)
    if reader is None:
        return {
            "state": "unknown",
            "turn_id": None,
            "reason": "unknown_provider",
            "source": "registry",
            "annotations": [],
            "diagnostics": [{"kind": "unknown_provider", "provider": provider}],
        }
    return reader.classify(session_log_text, process=process)


def read_fault_facts(provider: str, records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Extract normalized fault/approval facts from already-parsed provider
    records, using the provider reader. The abnormal track consumes these
    without naming a provider.
    """
    reader = _reader_for(provider)
    if reader is None or not hasattr(reader, "extract_facts"):
        return []
    facts, _diag = reader.extract_facts(records or [])
    fault_kinds = {"error", "failed", "approval"}
    out: list[dict[str, Any]] = []
    for fact in facts:
        if fact.get("kind") in fault_kinds:
            enriched = dict(fact)
            enriched.setdefault("provider", provider)
            out.append(enriched)
    return out


def _reader_for(provider: str, registry: Any = None) -> Any:
    provider = _reader_provider(provider)
    if provider in _READER_CACHE:
        return _READER_CACHE[provider]
    entry = None
    if isinstance(registry, dict):
        entry = registry.get(provider) if provider in registry else registry
    if not isinstance(entry, dict) or "reader_module" not in entry:
        entry = get_provider_registry(provider)
    if not isinstance(entry, dict):
        return None
    module_name = entry.get("reader_module")
    if not module_name:
        return None
    try:
        module = importlib.import_module(module_name)
    except ImportError:
        return None
    _READER_CACHE[provider] = module
    return module


def _reader_provider(provider: str) -> str:
    return "claude" if provider == "claude_code" else provider


__all__ = ["read_turn_state", "read_fault_facts", "get_provider_registry"]
