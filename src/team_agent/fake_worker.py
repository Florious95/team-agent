from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from team_agent.runtime import report_result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--agent-id", required=True)
    args = parser.parse_args()
    workspace = Path(args.workspace)
    print(f"TEAM_AGENT_FAKE_READY agent={args.agent_id}", flush=True)
    rendered_message: list[str] | None = None
    for line in sys.stdin:
        line = line.strip()
        if not line:
            if rendered_message is not None:
                rendered_message.append("")
            continue
        print(f"TEAM_AGENT_FAKE_WORKING agent={args.agent_id}", flush=True)
        if line.startswith("TEAM_AGENT_MESSAGE "):
            payload = json.loads(line.removeprefix("TEAM_AGENT_MESSAGE "))
            _report_fake_result(workspace, args.agent_id, payload)
        elif line.startswith("Team Agent message from "):
            rendered_message = [line]
        elif rendered_message is not None:
            rendered_message.append(line)
            if line.startswith("[team-agent-token:"):
                payload = _parse_rendered_message(rendered_message)
                _report_fake_result(workspace, args.agent_id, payload)
                rendered_message = None
        print(f"TEAM_AGENT_FAKE_READY agent={args.agent_id}", flush=True)


def _parse_rendered_message(lines: list[str]) -> dict[str, str | None]:
    header = lines[0]
    match = re.match(r"Team Agent message from (?P<sender>[^:]+?)(?: for (?P<task_id>[^:]+))?:$", header)
    token_line = next((line for line in lines if line.startswith("[team-agent-token:")), "[team-agent-token:manual]")
    token = token_line.removeprefix("[team-agent-token:").removesuffix("]")
    content_lines = [line for line in lines[1:] if not line.startswith("[team-agent-token:")]
    content = "\n".join(content_lines).strip()
    return {
        "message_id": token,
        "task_id": match.group("task_id") if match else None,
        "from": match.group("sender") if match else "leader",
        "content": content,
    }


def _report_fake_result(workspace: Path, agent_id: str, payload: dict) -> None:
    task_id = payload.get("task_id") or "manual"
    envelope = {
        "schema_version": "result_envelope_v1",
        "task_id": task_id,
        "agent_id": agent_id,
        "status": "success",
        "summary": f"Fake worker handled message {payload['message_id']}",
        "changes": [],
        "tests": [{"command": "fake-provider", "status": "passed"}],
        "risks": [],
        "artifacts": [
            {
                "path": str(workspace / ".team" / "logs" / f"{agent_id}.scrollback"),
                "description": "tmux scrollback for fake worker",
            }
        ],
        "next_actions": [],
    }
    report_result(workspace, envelope)
    print(json.dumps(envelope), flush=True)


if __name__ == "__main__":
    main()
