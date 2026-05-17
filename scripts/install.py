from __future__ import annotations

import argparse
import glob
import os
import shutil
import subprocess
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description="Install local team-agent wrapper commands")
    parser.add_argument("--prefix", default=str(Path.home() / ".local"), help="Install prefix; bin/ is created under it")
    parser.add_argument("--python", help="Python executable to embed in wrappers; defaults to TEAM_AGENT_PYTHON, python3, then python")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    python = _resolve_python(args.python)
    bin_dir = Path(args.prefix) / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    _write_wrapper(bin_dir / "team-agent", repo, "team_agent", python)
    _write_wrapper(bin_dir / "team_orchestrator", repo, "team_agent.mcp_server", python)
    _write_wrapper(bin_dir / "team-agent-coordinator", repo, "team_agent.coordinator", python)
    print(f"installed: {bin_dir / 'team-agent'}")
    print(f"installed: {bin_dir / 'team_orchestrator'}")
    print(f"installed: {bin_dir / 'team-agent-coordinator'}")
    print(f"python: {python}")
    print(f"ensure PATH contains: {bin_dir}")


def _resolve_python(explicit: str | None) -> str:
    candidates = _python_candidates(explicit)
    for candidate in candidates:
        if not candidate:
            continue
        resolved = shutil.which(candidate) if os.path.basename(candidate) == candidate else candidate
        if not resolved:
            continue
        proc = subprocess.run(
            [
                resolved,
                "-c",
                "import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)",
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        if proc.returncode == 0:
            return resolved
    raise SystemExit("No usable Python >= 3.10 found. Set TEAM_AGENT_PYTHON or pass --python.")


def _python_candidates(explicit: str | None) -> list[str]:
    candidates = [
        explicit,
        os.environ.get("TEAM_AGENT_PYTHON"),
        "python3",
        "python",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
        "/opt/homebrew/opt/python@3/bin/python3",
        "/usr/local/opt/python@3/bin/python3",
    ]
    candidates.extend(glob.glob("/opt/homebrew/opt/python@*/bin/python3*"))
    candidates.extend(glob.glob("/usr/local/opt/python@*/bin/python3*"))
    seen: set[str] = set()
    result: list[str] = []
    for item in candidates:
        if not item or item in seen:
            continue
        seen.add(item)
        result.append(item)
    return result


def _write_wrapper(path: Path, repo: Path, module: str, python: str) -> None:
    path.write_text(
        "#!/usr/bin/env sh\n"
        f'PYTHON_BIN="${{TEAM_AGENT_PYTHON:-{python}}}"\n'
        f'PYTHONPATH="{repo / "src"}" exec "$PYTHON_BIN" -m {module} "$@"\n',
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | 0o755)


if __name__ == "__main__":
    main()
