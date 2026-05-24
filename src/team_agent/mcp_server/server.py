from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from team_agent.mcp_server.contracts import TOOLS
from team_agent.mcp_server.tools import TeamOrchestratorTools


def dispatch(tools: TeamOrchestratorTools, request: dict[str, Any]) -> dict[str, Any]:
    tool = request.get("tool") or request.get("method")
    args = request.get("arguments") or request.get("params") or {}
    if tool == "assign_task":
        return tools.assign_task(**args)
    if tool == "send_message":
        return tools.send_message(**args)
    if tool == "report_result":
        return tools.report_result(**args)
    if tool == "update_state":
        return tools.update_state(**args)
    if tool == "get_team_status":
        return tools.get_team_status()
    if tool == "stop_agent":
        return tools.stop_agent(**args)
    if tool == "reset_agent":
        return tools.reset_agent(**args)
    if tool == "add_agent":
        return tools.add_agent(**args)
    if tool == "fork_agent":
        return tools.fork_agent(**args)
    if tool == "request_human":
        return tools.request_human(**args)
    return {"ok": False, "error": f"unknown tool {tool!r}"}


def handle_mcp(tools: TeamOrchestratorTools, request: dict[str, Any]) -> dict[str, Any] | None:
    method = request.get("method")
    msg_id = request.get("id")
    if method and method.startswith("notifications/"):
        return None
    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "protocolVersion": request.get("params", {}).get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "team_orchestrator", "version": "0.1.4"},
            },
        }
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}}
    if method == "tools/call":
        params = request.get("params", {})
        name = params.get("name")
        arguments = params.get("arguments") or {}
        try:
            result = dispatch(tools, {"tool": name, "arguments": arguments})
        except (TypeError, ValueError) as exc:
            result = {"ok": False, "reason": "invalid_tool_arguments", "error": str(exc)}
        except Exception as exc:
            result = {"ok": False, "reason": "internal_runtime_error", "error": str(exc)}
        is_error = result.get("ok") is False
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": json.dumps(result, ensure_ascii=False),
                    }
                ],
                "isError": is_error,
            },
        }
    return {
        "jsonrpc": "2.0",
        "id": msg_id,
        "error": {"code": -32601, "message": f"unknown method {method!r}"},
    }


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description="TeamSpec team_orchestrator MCP stdio server")
    parser.add_argument("--workspace", default=".", help="Workspace containing .team/runtime")
    args = parser.parse_args(argv)
    tools = TeamOrchestratorTools(Path(args.workspace))
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            if request.get("jsonrpc") == "2.0":
                response = handle_mcp(tools, request)
                if response is None:
                    continue
                sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
                sys.stdout.flush()
                continue
            result = dispatch(tools, request)
            sys.stdout.write(json.dumps({"ok": result.get("ok", True), "result": result}, ensure_ascii=False) + "\n")
            sys.stdout.flush()
        except Exception as exc:  # MCP transports need errors surfaced on stdout.
            if "request" in locals() and isinstance(request, dict) and request.get("jsonrpc") == "2.0":
                sys.stdout.write(
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "id": request.get("id"),
                            "error": {"code": -32000, "message": str(exc)},
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
            else:
                sys.stdout.write(json.dumps({"ok": False, "error": str(exc)}, ensure_ascii=False) + "\n")
            sys.stdout.flush()
