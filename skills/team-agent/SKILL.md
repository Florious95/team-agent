---
name: team-agent
description: Use only when the user explicitly asks to start, operate, inspect, shutdown, or restart a Team Agent team. Treat the team-agent CLI as a sealed appliance.
---

# Team Agent

Use this skill only for Team Agent operation. The leader is the current user-facing agent; do not create a `leader` worker. Worker role docs live in `<workspace>/agents/`; `TEAM.md` lives at `<workspace>/TEAM.md`.

## Leader Requirement

Real Team Agent teams require the current leader to run inside a tmux-managed pane. Prefer the short launchers:

```bash
team-agent codex
team-agent claude
```

Pass provider flags after the provider name, for example `team-agent codex --dangerously-bypass-approvals-and-sandbox`. Existing tmux layouts are valid too, including Finder/Ghostty launchers, as long as `team-agent quick-start` is invoked from the leader's current tmux pane. Do not start a real team from a naked terminal that Team Agent cannot address through tmux.

## Leader Role

Invoking this skill turns the current agent into the team leader. The leader **orchestrates**: read reports, set direction, decompose work, dispatch tasks to teammates, review results, and decide. The leader does **not** execute hands-on work — no `cargo test`, no product-code edits, no `git push`, no build/verify cycles. Those belong to teammates. If the leader catches themselves running tests, editing source files, or pushing commits, they have stepped out of role; stop and re-dispatch.

When the user has been communicating in Chinese throughout the conversation, all leader↔teammate messaging (`send`, `report_result`, MCP messages, task descriptions) must also be in Chinese. The leader dispatches in Chinese, the worker reports back in Chinese. Switch back to the user's language only at the user-facing boundary.

## Minimal Copy-Paste Team

```bash
mkdir -p .team/current/agents
team-agent profile init codex-default --auth-mode subscription --workspace .
cat > .team/current/TEAM.md <<'EOF'
---
name: demo-team
objective: One worker handles bounded tasks and reports through Team Agent MCP.
dangerous_auto_approve: false
fast: false
provider_models:
  codex: gpt-5.5
  claude: claude-sonnet-4-6
  claude_code: claude-sonnet-4-6
---

Team config only. This is not a worker role.
EOF
cat > .team/current/agents/coder.md <<'EOF'
---
name: coder
role: Implementation Worker
provider: codex
auth_mode: subscription
profile: codex-default
tools:
  - fs_read
  - fs_list
  - fs_write
  - execute_bash
  - mcp_team
  - provider_builtin
---

Handle one bounded task at a time. Send progress to leader only when needed. Final completion must call report_result exactly once; MCP fills task ids and result envelope fields.
EOF
team-agent quick-start .team/current
```

YAML lists must be block style. Use `tools:\n  - fs_read`; do not use `tools: [fs_read, mcp_team]`.

Display choices (set `display_backend:` in `TEAM.md` to opt in):

- `none` (default): headless / no GUI window manager. The team runs entirely in the per-workspace tmux server; this is what the demo above uses.
- `adaptive`: framework picks an available GUI layout for the local platform.
- `ghostty_workspace`: one Ghostty window. Workers are shown in tmux tabs/windows, up to 3 side-by-side panes per tab. Four workers become `3 + 1`; eight become `3 + 3 + 2`.
- `ghostty_window`: one Ghostty window per worker.

**Omitting `display_backend` defaults to `none`** (changed in 0.3.4). Set `display_backend: adaptive` (or one of the explicit ghostty variants) in `TEAM.md` only when the user wants GUI windows.

## Private Tmux Socket

Worker windows live on a private per-workspace tmux server, not the user's default socket. `tmux list-sessions` (no `-L`/`-S`) will not show them; that is expected, not a failure.

To attach manually, read `attach_commands` (or the `tmux` action printed near `ready:`) from `team-agent quick-start` / `team-agent restart` / `team-agent status --json` output. It is the canonical `tmux -L <socket-name> attach -t <session>` (or `-S <socket-path>`) line for the current team.

Use `team-agent attach-leader` / `team-agent claim-leader` to bind the leader pane to a team. Do not invent socket paths by hand.

## Provider Capability Matrix

| Provider | Resume | Turn-state detection | Per-worker model override | Native session fork |
|---|---|---|---|---|
| `claude` / `claude_code` | yes (`--resume <id>`, transcript-verified) | yes (JSONL stream) | yes (role `model` overrides `provider_models`) | yes (snapshot copy + only `--resume <snapshot-id>`) |
| `codex` | yes (`codex resume <id>`, session-store-verified) | yes (turn JSONL) | yes (role `model`) | yes (`codex fork`) |
| `copilot` | yes (`copilot --resume <id|name>`, sqlite `sessions` row) | not yet (phase 1: `provider.classify.unsupported` event) | yes (role `model`) | yes (isolated `COPILOT_HOME` store fork) |
| `gemini_cli` | no | no | yes | no |
| `fake` (testing only) | no | no | n/a | no |

Notes:
- Per-worker model override means a role-doc `model:` value wins over `TEAM.md` `provider_models.<provider>`; subscription defaults still fill blanks.
- Copilot fork copies the source session into an isolated `COPILOT_HOME` and rekeys its SQLite session references atomically. Missing or incomplete backing fails closed; it never falls back to a fresh spawn.
- Copilot phase-1 idle/turn detection is intentionally Unknown; tick emits a single explicit `provider.classify.unsupported` event per state change (P4 dedup), never a silent default.

## Provider Prep

### Subscription auth (Codex / Claude account login)

Before workers can use a subscription provider, create a named subscription profile in the workspace and reference it from role docs:

```bash
team-agent profile init codex-default --auth-mode subscription --workspace .
team-agent profile init claude-default --auth-mode subscription --workspace .
```

Then in `agents/<role>.md` frontmatter, set `auth_mode: subscription` and `profile: codex-default` (or `claude-default`). The demo above uses `profile: codex-default`; that name only works after `profile init` has created it in the same workspace.

Common errors:

- `profile already exists`: a profile by that name is already in `.team/current/profiles/`. Either reuse it (skip `init`) or pick a new name.
- `profile not found` during quick-start: the role doc references a profile that was never `profile init`-ed in this workspace. Run `team-agent profile init <name> --auth-mode subscription --workspace .` and retry.

### Codex provider notes

Codex: run `codex login` first. Optional `~/.codex/config.toml` profile:

```toml
[profiles.team-agent]
model = "gpt-5.5"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
```

Use exact provider model ids, not display names. For Codex workers, the model must match a `slug` from `codex debug models`; for example use `gpt-5.3-codex-spark`, not `GPT-5.3-Codex-Spark`.
Role docs may omit `model` for subscription workers. Team Agent fills subscription defaults from `TEAM.md` `provider_models`, then built-in provider defaults (`codex: gpt-5.5`, `claude/claude_code: claude-sonnet-4-6`). Use role-level `model` only for intentional per-worker overrides.

Claude: run `claude auth status`; if missing, run `claude auth login`. Team Agent stores Claude worker sessions by passing `--session-id` and resumes with `--resume`.
Use `provider: claude` or `provider: claude_code` for Claude workers.

If the current leader process was started with `claude --dangerously-skip-permissions` or `codex --dangerously-bypass-approvals-and-sandbox`, Team Agent inherits that permission mode for worker launch, restart, and single-agent repair.
Role `profile` values are secret-safe references. Do not put API keys in role docs or `TEAM.md`.
Never read raw provider profile files into model context. Do not use `Read`, `cat`, `sed`, `grep`, editors, or screenshots on `.team/current/profiles/*.env` or `.team/runtime/provider-env/*.env`. Those files may contain live API keys. Use only `team-agent profile show <name> --workspace . --json` or `team-agent profile doctor <name> --workspace . --json` for redacted diagnostics; if a value is missing, ask the human user to edit the local profile file.
When the user asks for a third-party or compatible API, do not ask them to paste keys into the chat. Generate a local blank profile instead, for example:

```bash
team-agent profile init deepseek --auth-mode compatible_api --workspace .
```

Tell the user to fill `.team/current/profiles/deepseek.env` locally:

```env
AUTH_MODE=compatible_api
PROFILE_NAME=deepseek
BASE_URL=
API_KEY=
MODEL=
```

Then reference only `auth_mode: compatible_api` and `profile: deepseek` in role docs. Do not invent or duplicate a role `model` when the profile already has `MODEL=`; if both places define a model they must match exactly. Team Agent loads the profile automatically during quick-start, launch, restart, and start-agent. Compatible API workers inherit the current shell proxy/CA environment by default. Claude compatible API workers use Team Agent managed `CLAUDE_CONFIG_DIR` so user-level Claude subscription settings cannot re-inject Anthropic proxy variables into third-party API sessions. If quick-start reports an ambient proxy blocker, do not silently unset proxy for the whole team; tell the user to choose one path: fix that proxy for `BASE_URL`, put `HTTPS_PROXY=`/`HTTP_PROXY=` in the profile, or put `PROXY_MODE=direct` in the profile to bypass proxy only for that worker. Subscription workers keep their native provider settings and environment. Startup runs a redacted smoke check for compatible API profiles before worker windows are created, so a bad URL/key/model or proxy/base URL connectivity failure is reported to the leader command instead of producing idle workers.
For diagnosis, run `team-agent profile show deepseek --workspace . --json`; never open the `.env` file to check whether `API_KEY` or `MODEL` is filled.

## Commands

- `team-agent codex ...` starts or attaches a tmux-managed Codex leader in the current directory; arguments after `codex` pass through to Codex.
- `team-agent claude ...` starts or attaches a tmux-managed Claude leader in the current directory; arguments after `claude` pass through to Claude.
- `team-agent quick-start .team/current` starts workers from `TEAM.md` and `agents/*.md`. When it prints `ready:` and `ready_signal`, startup is complete; do not run sleep/status/wait loops afterward unless diagnosing a failure.
- For real workers, `quick-start` requires a current tmux leader pane. If it says the leader must run inside tmux, restart the leader with `team-agent codex`/`team-agent claude` or use an existing tmux-managed layout, then run quick-start again.
- Quick-start generated files stay inside the selected team directory, for example `.team/current/` or `.team/alpha/`; do not create or expect root `team.spec.yaml` or `team_state.md`.
- Use `team-agent quick-start ./roles --team-id alpha` to create a second generated team under `.team/alpha/`, or pass an existing team directory directly such as `team-agent quick-start .team/alpha`.
- `quick-start` is only for first-time team creation from role docs. If that team already has runtime state, use `team-agent restart . --team <session_name_or_team_name>` to resume it. If restart cannot recover context, explain the loss and wait for explicit user consent before using `team-agent restart . --allow-fresh`; never reset context through quick-start.
- If the user explicitly asks a worker to create or operate a nested child team, first read `references/team-in-team.md`. Child teams must use an independent child workspace, never the parent `.team/current`.
- `team-agent send --watch-result coder "Do the bounded task"` sends a direct worker message, returns after delivery, and lets the coordinator collect/report completion asynchronously.
- After `send --watch-result` succeeds, do not run `sleep`, `status`, `inbox`, or `collect` polling loops unless the user explicitly asks for diagnosis; the coordinator will notify the leader when the result arrives.
- `team-agent send --task task_initial "Start"` routes by task.
- `team-agent status` shows team, worker health, result-store counts, `session_id`, `captured_via`, and attribution confidence. `team-agent status --json` is compact and context-safe by default; use `team-agent status --detail --json` only for raw runtime-state diagnostics.
- `team-agent status coder` shows one worker.
- `team-agent approvals [coder]` shows structured pending approval prompts without copying worker terminal pages.
- `team-agent inbox coder` shows message history only. Final results are not in inbox.
- `team-agent shutdown --workspace . --keep-logs` stops the tmux session after a final session capture attempt.
- `team-agent restart .` restarts a stopped team from stored worker sessions. If one workspace has multiple restartable teams, use `team-agent restart . --team <session_name_or_team_name>`.
- `team-agent start-agent coder --workspace .` repairs one missing worker window without interrupting other workers.
- `team-agent doctor` checks local dependencies and provider auth hints.

## Restart Semantics

`restart` takes one workspace argument. It preserves each worker's original provider. If a verified provider session exists, the worker resumes (`codex resume <id>` or `claude --resume <id>`). Claude sessions are considered resumable only after the provider has written a project transcript for that session; a freshly opened blank Claude window is not recorded as recovered context. If the stored id is stale, the runtime first tries to repair it from verified transcript history. If a stored session cannot be verified or repaired, restart fails closed instead of silently losing context; use `team-agent restart . --allow-fresh` only when the user explicitly accepts a fresh worker context. If multiple stopped teams in the same workspace have restart context, plain `team-agent restart .` fails and lists candidates; rerun with `--team <session_name_or_team_name>`. If no prior session id exists, that worker starts fresh and the event log records `restart.fresh_spawn`. Claude resume must run from the original cwd and the same provider transcript root; Team Agent stores `spawn_cwd` and compatible-API `claude_projects_root` for that.

Startup trust prompts are handled by the runtime/coordinator with bounded probes; do not wait on raw worker screens or manually press Enter for routine startup trust prompts.

Use `team-agent start-agent <agent_id> --workspace .` only as a narrow repair when one worker window is missing after launch/restart/display failure. It preserves the worker provider, resumes from `session_id` when available, starts fresh when there is no prior session id, and does not restart the rest of the team. If an existing session id cannot resume, it fails closed unless the user explicitly passes `--allow-fresh`.

## Adding A New Worker At Runtime

To add a new worker to a running team, write the role doc and run **one command** — do not shutdown/restart, do not regenerate the compiled spec, and do not quick-start an existing team:

```bash
cat > .team/current/agents/reviewer.md <<'EOF'
---
name: reviewer
role: Code Reviewer
provider: codex
auth_mode: subscription
profile: codex-default
tools:
  - fs_read
  - fs_list
  - mcp_team
---

Review changed files and report findings to leader.
EOF
team-agent add-agent reviewer --role-file .team/current/agents/reviewer.md --workspace .
```

`add-agent` registers the new worker into the running team's state, launches its window on the existing tmux socket, and leaves every other worker untouched. **Do not shutdown/restart for adding a worker** — it loses every other worker's resumable session. If `add-agent` fails, surface the structured error to the user; do not fall back to shutdown.

Semantic distinction:

- `team-agent add-agent <agent> --role-file <file>` — add a **new** worker not yet in team state.
- `team-agent clone-agent <source> --as <new>` — reread the source worker's latest role file and start a fresh provider seat. It never copies conversation context. Success is initially honest `capture_state: pending_first_turn` with `session_id`, `new_session_id`, and `backing_path` all null; after the first turn, canonical capture changes the state to `captured` and fills the backing tuple.
- `team-agent fork-agent <source> --as <new>` — reread the same latest role file and create a distinct, verified provider session that forks the source context. If the provider backing cannot be verified, the command fails and rolls back instead of silently cloning fresh.
- `team-agent start-agent <agent>` — (re)launch a worker that **already exists** in team state but whose window is missing.
- `team-agent reset-agent <agent> --discard-session` — keep the same seat and deliberately start it with fresh context.
- `team-agent restart .` — resume a fully **stopped** team from stored worker sessions.
- `team-agent quick-start <dir>` — first-time team creation from role docs; for existing teams use `restart`, and use `restart --allow-fresh` only after explicit user consent to discard context.

Clone/fork names are always explicit: run concurrent calls with a different `--as` value for each new seat. Fork success includes a verified new `session_id` and independent backing; a tmux window alone is not fork success. Clone success uses the honest `pending_first_turn` state above until first-turn capture, never a fabricated verified tuple. Updating the source role file affects the next clone/fork without requiring a full-team rebuild. Automatic knowledge write-back from a clone/fork into the source role file is not provided.

Removing a worker at runtime is the symmetric `team-agent remove-agent <agent> --workspace . --confirm`.

## Worker Protocol

Workers normally do not run nested Team Agent teams. When the user or leader explicitly asks for a child team, follow `references/team-in-team.md`; otherwise workers only provide the target and content for progress, and a short completion summary at the end:

```text
team_orchestrator.send_message(to="leader", content="short progress or blocker")
# to another teammate:
team_orchestrator.send_message(to="<agent_id>", content="short coordination note")
# to every other team member:
team_orchestrator.send_message(to="*", content="short broadcast")
team_orchestrator.report_result(summary="short completion", status="success", tests=[{"command":"command","status":"passed"}])
```

Do not pass `sender`, `task_id`, `requires_ack`, `schema_version`, or `agent_id` unless doing a low-level compatibility diagnostic. The MCP runtime fills those fields and keeps delivery metadata in runtime state and event logs. If provider env loses the worker id, MCP infers it from active task/message state and falls back to an explicit `unknown` sender instead of treating the worker as leader.

Message targets are team-scoped. Use `leader`, another teammate agent id, or `*` for all other team members. The runtime excludes the sender from `*` broadcasts and never scans unrelated terminal windows for recipients.

`report_result` stores final completion and immediately attempts a leader notification through the verified/fallback delivery path. `team-agent collect` remains the authoritative state-update path. Do not wait for final results through `team-agent inbox`, message ack counts, or repeated plain status polling. `acknowledged_count` only means prior task messages were acknowledged by the worker; it is not a missing-result signal.
For normal leader dispatch, prefer `team-agent send --watch-result ...`; when it returns a registered watcher notice, the framework will notify the leader at completion.

For long processes, workers must write logs, keep a pid, provide a health check, and stop after a bounded number of retries. QA/reviewer roles must stay within their authorized files and stop on service unavailable, approval prompts, or repeated startup failure.

## Failure Rules

For any non-zero `team-agent` exit, report the command, exit code, last about 20 stderr lines, and affected task or agent when known. Then stop and wait for the user.

Do not retry with changed flags. Do not inspect source code or private runtime state. Do not operate tmux directly except when the user asks for a manual diagnostic. Do not answer provider approval prompts for the user.

If `quick-start` reports `tmux session already exists`, treat it as a team-name collision. The existing session may be an active team; do not terminate it and do not suggest `shutdown` as the normal fix. Change `name:` in `TEAM.md` so the next launch uses a different tmux session name, then run `team-agent quick-start .team/current` again.

Known Team Agent control-plane MCP prompts such as `team_orchestrator.report_result` and `team_orchestrator.send_message` are handled by the coordinator. It uses session-scoped approval, verifies the prompt cleared, retries boundedly, and logs the result. Do not ask the user to approve those routine internal prompts.

When `status` still shows `AWAITING_APPROVAL`, run `team-agent approvals <agent_id>`, show the structured prompt summary and choices, ask the user to decide, and wait.

Do not inspect raw worker terminal output during normal operation. Use `team-agent status`, `team-agent approvals`, `team-agent inbox`, `team-agent collect`, and event logs instead. Raw-screen diagnostics are outside this skill's normal workflow, require explicit user authorization, and are guarded by the CLI; use them only as a one-shot bounded diagnostic, never as a routine workflow step.

For "worker reported but leader cannot see completion":

1. Run `team-agent collect` once; this is the final-result intake path.
2. If no result is collected, inspect `team-agent status --json` field `results`. `uncollected > 0` means the result is already accepted by MCP and waiting in the result store.
3. Check `.team/logs/events.jsonl` for `mcp.report_result` and `collect.result` before sending another prompt to the worker.
4. Do not loop on `team-agent inbox` or ack/status counts; that burns context and cannot consume final results.
