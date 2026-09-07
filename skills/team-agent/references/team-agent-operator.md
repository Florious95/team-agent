# Team Agent operator handbook

This is the long-form operator reference. The sealed CLI face is `../SKILL.md` (≤60 lines). Do not drop the r13-corrected 0.5.66 facts below.


# Team Agent

Use this skill only for Team Agent operation. The leader is the current user-facing agent; do not create a `leader` worker. Worker role docs live in `<workspace>/agents/`; `TEAM.md` lives at `<workspace>/TEAM.md`.

If `team-agent --version` does not match `last_verified_against` in the frontmatter, **do not copy examples from this file as current truth**. Learn from the live CLI first:

```bash
team-agent --version
team-agent --help
team-agent doctor --help
```

Observed on the writing gauge: `team-agent 0.5.66` (exit 0); `doctor --help` prints `usage: team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--gate orphans|comms] [--comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]` (exit 0).

This page documents **installed CLI 0.5.66**. Claims below are paired with a command you can re-run; do not fill gaps by `strings` on the binary. **Never rewrite a user-supplied model id.** Only verify and report.

Gauge used while writing (re-check on your machine):

```text
/Users/alauda/.team-agent/runtime/0.5.66/bin/team-agent
md5=5dfc5fc770eb91d8104ab503704112ec
mtime=2026-08-20T03:06:49
team-agent 0.5.66
```

```bash
team-agent --version
```

Observed: `team-agent 0.5.66` (exit 0).

## Leader Requirement

Real Team Agent teams require the current leader to run inside a tmux-managed pane. Prefer the short launchers:

```bash
team-agent codex
team-agent claude
```

Pass provider flags after the provider name, for example `team-agent codex --dangerously-bypass-approvals-and-sandbox`. Existing tmux layouts are valid too, including Finder/Ghostty launchers, as long as `team-agent quick-start` is invoked from the leader's current tmux pane. Do not start a real team from a naked terminal that Team Agent cannot address through tmux.

**0.5.66:** leader `--dangerously-*` flags no longer inherit bypass to workers. Per-worker bypass is the role field `dangerously_skip_permissions` (see Permissions).

## Leader Role

Invoking this skill turns the current agent into the team leader. The leader **orchestrates**: read reports, set direction, decompose work, dispatch tasks to teammates, review results, and decide. The leader does **not** execute hands-on work — no `cargo test`, no product-code edits, no `git push`, no build/verify cycles. Those belong to teammates. If the leader catches themselves running tests, editing source files, or pushing commits, they have stepped out of role; stop and re-dispatch.

When the user has been communicating in Chinese throughout the conversation, all leader↔teammate messaging (`send`, `report_result`, MCP messages, task descriptions) must also be in Chinese. The leader dispatches in Chinese, the worker reports back in Chinese. Switch back to the user's language only at the user-facing boundary.

## Minimal Copy-Paste Team

`dangerously_skip_permissions` is **required** on every role doc (boolean). `TEAM.md` `dangerous_auto_approve` is **not** the 0.5.66 bypass switch (see Permissions). Values under `provider_models:` below are **fill-ins for omitted role `model`**, not a validity catalog (lookup: `team-agent compile --team <dir> --out /tmp/spec.yaml --json` and read the compiled agent `model`).

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
dangerously_skip_permissions: false
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

That copy-paste role is Codex-specific. Provider tool categories are not portable: Pi roles must contain `mcp_team` and may use only `fs_read`, `fs_list`, `fs_write`, and `execute_bash`; do not carry `provider_builtin`, `git_diff`, or `network` into a Pi role. `team-agent compile`/`validate` rejects an unsupported Pi category before quick-start persists runtime state, and the error names the category to remove.

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

```bash
team-agent claim-leader --help
```

Observed (exit 0):

```text
usage: team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]
```

Run `claim-leader` only from the **leader** pane. From a worker pane it refuses and prints a structured `action` (see Failure Rules).

## Provider Capability Matrix

| Provider | Resume | Turn-state detection | Per-worker model override | Native session fork |
|---|---|---|---|---|
| `claude` / `claude_code` | yes (`--resume <id>`, transcript-verified) | yes (JSONL stream) | yes (role `model` overrides `provider_models`) | yes (snapshot copy + only `--resume <snapshot-id>`) |
| `codex` | yes (`codex resume <id>`, session-store-verified) | yes (turn JSONL) | yes (role `model`) | yes (`codex fork`) |
| `copilot` | yes (`copilot --resume <id|name>`, sqlite `sessions` row) | not yet (phase 1: `provider.classify.unsupported` event) | yes (role `model`) | yes (isolated `COPILOT_HOME` store fork) |
| `gemini_cli` | no | no | yes | no |
| `fake` (testing only) | no | no | n/a | no |

Notes:
- Per-worker model override means a role-doc `model:` value wins over `TEAM.md` `provider_models.<provider>` at **compile** time; subscription defaults still fill blanks when there is no profile-deferred null.
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

Use exact provider model ids, not display names. **Codex slug lookup is Codex-only** — do not apply it to `compatible_api` workers.

```bash
codex debug models
```

That command is the Codex catalog. This skill does **not** keep a model id list. If `codex` is not on PATH, the lookup is unavailable; do not invent slugs.

Role docs may omit `model` for non-grok subscription workers; compile then fills from `TEAM.md` `provider_models` or leaves `null` when a profile is present (see compatible_api). **Do not treat those fill-ins as a validity catalog.** **Grok roles cannot omit `model`** — compile fails closed (same `compile --team` command as below).

Claude: run `claude auth status`; if missing, run `claude auth login`. Team Agent stores Claude worker sessions by passing `--session-id` and resumes with `--resume`.
Use `provider: claude` or `provider: claude_code` for Claude workers.

Role `profile` values are secret-safe references. Do not put API keys in role docs or `TEAM.md`.
Never read raw provider profile files into model context. Do not use `Read`, `cat`, `sed`, `grep`, editors, or screenshots on `.team/current/profiles/*.env` or `.team/runtime/provider-env/*.env`. Those files may contain live API keys. Use only `team-agent profile show <name> --workspace . --json` or `team-agent profile doctor <name> --workspace . --json` for redacted diagnostics; if a value is missing, ask the human user to edit the local profile file.

## compatible_api model rules

**Only verify. Only report. Never rewrite a user-explicit model id** (role `model` or profile `MODEL`). Do not keep a catalog of legal ids in this skill. Compatible-API ids are whatever the endpoint serves.

When the user asks for a third-party API, do not ask them to paste keys into the chat. Generate a local blank profile:

```bash
team-agent profile init deepseek --auth-mode compatible_api --workspace .
```

Tell the user to fill `.team/current/profiles/deepseek.env` locally (`AUTH_MODE`, `PROFILE_NAME`, `BASE_URL`, `API_KEY`, `MODEL`). Then reference `auth_mode: compatible_api` and `profile: deepseek` in the role doc.

Inspect without opening the `.env`:

```bash
team-agent profile show deepseek --workspace . --json
```

Observed on a freshly inited profile (exit 0): `ok: true`, `auth_mode: "compatible_api"`, `keys_present` includes `MODEL` and `BASE_URL`, `secret_values_printed: false`.

How to see whether the **endpoint** accepts the id (ask the human to run these with their `BASE_URL`; do not read the `.env` yourself):

```bash
# 1) live catalog (OpenAI-shaped). Replace BASE_URL. Do not treat HTTP errors as "illegal id".
curl -sS --max-time 5 -H "Authorization: Bearer $API_KEY" "$BASE_URL/v1/models"
```

Writing-gauge shape check without a real endpoint. `--noproxy '*'` pins a direct connect so ambient HTTP(S)_PROXY cannot rewrite the process exit. This is a **liveness probe of the lookup command**, not a model list:

```bash
curl --noproxy '*' -sS --max-time 5 http://127.0.0.1:1/v1/models
```

Observed (exit 7): stderr contains `Couldn't connect to server` / `Failed to connect to 127.0.0.1 port 1`. Gauge `/usr/bin/curl` (md5 `10a716a5b63881997d819ef693d4a802`). Port 1 is not a service.

Dropping `--noproxy` is a different environment: an intercepting proxy may return an HTML error page (a prior writing seat saw Squid `ERR_CONNECT_FAIL`) and the process exit can be 0. This turn did not re-measure that intercept. Do not record 0 as this `--noproxy` fence's expected exit.

If the endpoint has no `/v1/models`, probe with a 1-token completion (human runs this; this writing turn did **not** hit a live chat endpoint):

```text
POST {BASE_URL}/v1/chat/completions
{"model":"<the user-supplied id>","messages":[{"role":"user","content":"ping"}],"max_tokens":1}
```

Fail = report the HTTP/body. Success = leave the id alone. **Do not substitute a different id.**

How compile currently wires role `model` vs profile `MODEL` (not a validity list):

```bash
team-agent compile --help
```

Observed (exit 0): `usage: team-agent compile --team TEAM [--out FILE] [--json]`.

Recreate: `auth_mode: compatible_api` without `profile` → compile exit 1 `profile is required when auth_mode is 'compatible_api'`. Role `model:` set + `profile:` set → compiled agent `model` is the role string; profile `MODEL` is not read at compile. Role omits `model` + `profile:` → compiled `model: null`. Compile does **not** compare the two. Whether launch refuses a mismatch was **not** spawned-verified.

How to check `--auth-mode` literals (the CLI does not print an allow-list on this gauge):

```bash
team-agent profile init x --auth-mode not-a-mode --workspace /tmp/ta-skill-no-runtime --json
```

Observed (exit 1): `usage error: invalid --auth-mode: not-a-mode`. That is the lookup: try the value, read the error. Do not maintain a mode table here.

Team Agent loads the profile during quick-start, launch, restart, and start-agent. Compatible API workers inherit the current shell proxy/CA environment by default. Claude compatible API workers use Team Agent managed `CLAUDE_CONFIG_DIR` so user-level Claude subscription settings cannot re-inject Anthropic proxy variables into third-party API sessions. If quick-start reports an ambient proxy blocker, do not silently unset proxy for the whole team; tell the user to choose one path: fix that proxy for `BASE_URL`, put `HTTPS_PROXY=`/`HTTP_PROXY=` in the profile, or put `PROXY_MODE=direct` in the profile to bypass proxy only for that worker. Subscription workers keep their native provider settings and environment. Startup runs a redacted smoke check for compatible API profiles before worker windows are created, so a bad URL/key/model or proxy/base URL connectivity failure is reported to the leader command instead of producing idle workers.

## How role-doc frontmatter becomes live

```bash
team-agent restart --help
team-agent add-agent --help
team-agent shutdown --help
```

Observed (all exit 0):

```text
usage: team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json]
usage: team-agent add-agent AGENT --role-file FILE [--force] [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]
usage: team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]
```

Top-level help names `add-agent` as **add or force-recreate a worker**. `restart` does **not** take `--role-file`. Editing `agents/*.md` then running `restart` is therefore not a documented reread path.

To apply a changed role file to an **existing** worker name, use `add-agent --force --role-file <file>` (the `--role-file` flag is the reread).

`shutdown` stops the selected team. `restart` restarts it. This pair was **not** live-verified as a frontmatter reread; do not assume it recompiles `agents/*.md`. Prefer `add-agent --force --role-file` when the help surface names `--role-file`.

`restart --force` is **not** listed in `--help`. Passing it to a workspace with no team is a team-select error, not a usage error — so the flag is accepted. Its live-team meaning is **unverified** on this gauge; do not invent it.

Do **not** shutdown/restart the whole team just to add a **new** worker (that drops other workers' resumable sessions). Use `add-agent` without `--force` for a new name.

## Permissions (0.5.66)

There is **no** `team-agent permission-modes` command.

```bash
team-agent permission-modes
```

Observed (exit 1): `invalid choice: 'permission-modes'`.

`team-agent --help` does **not** list `permission_mode`, `TEAM_AGENT_LEADER_BYPASS`, or `dangerous_auto_approve`. Lookup is `team-agent compile` / `team-agent validate` on a throwaway team dir, not a flag enum.

### Live control: role `dangerously_skip_permissions`

Required boolean on every role doc. Missing or non-bool fails compile.

Recreate:

```bash
d=$(mktemp -d) && mkdir -p "$d/agents"
printf '%s\n' '---' 'name: t' 'objective: t' 'provider: fake' '---' 'x' > "$d/TEAM.md"
printf '%s\n' '---' 'name: coder' 'role: Worker' 'provider: fake' 'model: fake' 'auth_mode: subscription' 'tools:' '  - mcp_team' '---' 'b' > "$d/agents/coder.md"
team-agent compile --team "$d" --out /tmp/ta-compile-out.yaml --json
```

Observed (exit 1): `missing front matter field dangerously_skip_permissions. This field must be declared explicitly; it controls whether the agent launches with permission prompts bypassed.` JSON also includes `"action": "run \`team-agent doctor\` or inspect the log path shown here"`.

Non-bool (`dangerously_skip_permissions: bypass`) observed (exit 1): `front matter field dangerously_skip_permissions must be a boolean.`

Legal values reachable from that error: **boolean `true` or `false`**. No other literals are accepted.

`true` is the per-worker bypass opt-in. `false` is the default you should copy unless the user explicitly wants prompts skipped.

### `permission_mode` (historical, not consumed)

A role may still contain `permission_mode`. Compile **does not** validate it and **does not** copy it into the compiled spec.

Observed: `permission_mode: bypass` and `permission_mode: not-a-mode` both compiled `ok: true` (exit 0); the spec agent block had `dangerously_skip_permissions: false` and **no** `permission_mode` key.

Do not set `permission_mode: bypass` expecting 0.5.66 bypass. Use `dangerously_skip_permissions: true`.

### `TEAM.md` `dangerous_auto_approve`

The demo `TEAM.md` still shows `dangerous_auto_approve: false`. Compile of a team with `dangerous_auto_approve: true` produced a spec **without** that key on the agent (only `dangerously_skip_permissions`). Treat it as leftover team YAML, not the bypass switch.

`dangerous_auto_approve` as a **tool sentinel** is internal (adapters). Operators do not configure it as a tools list item.

### `TEAM_AGENT_LEADER_BYPASS`

Not in `team-agent --help`. Not a user-facing config key. Do not export it by hand. Worker env injection was **not** live-spawned on this gauge.

### Leader argv bypass

Leader `--dangerously-*` is detected only as a **warning**, not as worker bypass (`detect_bypass_flag_in_argv` — "只做检测,不做行为决策"). Declare `dangerously_skip_permissions` per role.

### Priority — 未找到单一裁决点

An external report ranked `permission_mode` > `TEAM_AGENT_LEADER_BYPASS` > `TEAM.md` `dangerous_auto_approve`. **That ranking is not in this tree.** Grep of `crates/` found **no** function that takes those three keys and picks a winner (search: `permission_mode.*TEAM_AGENT_LEADER_BYPASS` and the reverse: zero hits).

What the sources actually do (reader can re-open these files):

| Symbol | File | What it does |
|---|---|---|
| `required_dangerously_skip_permissions` | `crates/team-agent/src/compiler.rs` | Compile **requires** role `dangerously_skip_permissions` bool. This is the 0.5.66 worker bypass source. |
| `resolved_tool_strings_for_command` | `crates/team-agent/src/lifecycle/worker_command_context.rs` | `true` appends tool sentinel `dangerous_auto_approve`; comment: no longer consumes team/runtime/leader `DangerousApproval`. |
| `provider_bypass_flag` | `crates/team-agent/src/provider/bypass_flags.rs` | **唯一权威**: role `dangerously_skip_permissions: true` decides whether to add a provider bypass argv flag. Maps provider → flag string only. |
| spec allowed key `permission_mode` | `crates/team-agent/src/model/spec.rs` | Historical; **not consumed** by compiler (comment: 0.5.66 起不再被 compiler 消费). CLI compile of `permission_mode: bypass` and `permission_mode: not-a-mode` both `ok: true` with the key absent from spec. |
| `apply_mcp_auto_approval_env` | `crates/team-agent/src/lifecycle/launch/worker_env.rs` | Writes `TEAM_AGENT_LEADER_BYPASS` `1`/`0` from `DangerousApproval` when source is `LeaderProcess`. Separate from compile. |
| `worker_spawn_env` test | `crates/team-agent/src/layout/worker_env.rs` | **Strips** inherited `TEAM_AGENT_LEADER_BYPASS` from parent env. |
| `claude_dangerous_auto_approve` (and siblings) | `crates/team-agent/src/provider/adapters/*.rs` | Adapters look for tool name `dangerous_auto_approve`, not `TEAM.md`. |

**Do not copy a three-row priority table.** The live operator knob on this gauge is role `dangerously_skip_permissions`. The three names from the report still exist as strings; they are not one ranked merge.

## Ignore vs handle (status / alerts)

```bash
team-agent status --help
```

Observed (exit 0):

```text
usage: team-agent status [AGENT] [--workspace WORKSPACE] [--team TEAM] [--summary|--json] [--detail]
默认输出: worker,空闲|工作|错误；错误细分走 status --summary
```

```bash
team-agent status --json --workspace /tmp/ta-skill-no-runtime
```

On a directory with **no** running team, observed (exit 0): `"ok": true`, `"ready": false`, `not_ready.reasons` included `coordinator_not_running`, `tmux_session_missing`, `workers_not_spawned`, `leader_receiver_unbound`, `next_action: claim-leader`. Create the dir if needed: `mkdir -p /tmp/ta-skill-no-runtime`.

That combination is **not** a crash. `ok: true` with `ready: false` means the command ran and the team is not ready.

| Signal | Treat as | What to do |
|---|---|---|
| `ok: true` + `ready: true` | normal | continue |
| `ok: true` + `ready: false` + `tmux_session_missing` / `coordinator_not_running` on a workspace you did not start | expected empty workspace | start the team, or pick the right `--workspace` / `--team` |
| `coordinator.session_missing` then `status --json` shows `ready: true` or workers still `running` | self-healing transient | do **not** shutdown; wait and re-check `ready` |
| `auto_attach_leader` / `leader pane validation failed` while the command still reports `ok: true` | info for external-leader setups (report §3.6) | ignore the word `failed`; look at `ok`. **This exact stderr string was not reproduced on this gauge.** |
| `result_success_without_executed_tests` on a non-coding task | advisory | ignore |
| structured `ok: false` with `action` | handle | run the `action` (see Failure Rules) |
| `caller_not_leader_shaped` from `claim-leader` | handle | run `claim-leader` from the leader pane, not a worker pane |

## coordinator.session_missing

`status --json` can list `tmux_session_missing` while `ok` is still true (observed on an empty workspace). For a **running** team, re-check:

```bash
team-agent status --json
```

If `ready` is true, or agents show `"status": "running"`, continue. Do not stop the team and wait for the user solely because a coordinator line said session missing.

Live-team sample on this writer seat (exit 0): agents including `developer-d108` with `"status": "running"`. That is the "still alive" side of the check.

## `--watch-result` is deprecated

```bash
team-agent send --help
```

Observed (exit 0):

```text
usage: team-agent send TO MESSAGE... [--workspace WORKSPACE] [--team TEAM] [--mailbox] [--json]

TO is a logical recipient; send returns after the message is persisted. --mailbox stores durably without live injection; omitted sends to the live conversation.
```

`--watch-result` is **not** in that usage. Passing it still warns:

```bash
mkdir -p /tmp/ta-skill-no-runtime
team-agent send --watch-result coder hello --workspace /tmp/ta-skill-no-runtime --json
```

Observed stderr (exit is the send result, here 1 because the probe dir had no runtime state):

```text
warning: --watch-result is deprecated; sunset: next compatibility release; action: use positional logical TO and the returned message id
```

**New write:** positional logical TO, then use the returned `message_id` (and `team-agent results --case` / coordinator notify). Do not poll `sleep` / `status` / `inbox` / `collect` after a successful send unless the user asked for diagnosis.

```text
team-agent send coder "Do the bounded task"
team-agent send reviewer "Review this change"
team-agent send /full/path/to/workspace::team-name/agent-id "cross-workspace note"
```

```bash
team-agent results --help
```

`results --help` observed (exit 0): `usage: team-agent results --case CASE_ID [--workspace WORKSPACE] [--team TEAM] [--json]`.

On success, `send --json` includes `message_id`. The deprecation warning itself does **not** print a full example (product backlog).

## Commands

- `team-agent codex ...` starts or attaches a tmux-managed Codex leader in the current directory; arguments after `codex` pass through to Codex.
- `team-agent claude ...` starts or attaches a tmux-managed Claude leader in the current directory; arguments after `claude` pass through to Claude.
- `team-agent quick-start .team/current` starts workers from `TEAM.md` and `agents/*.md`. When it prints `ready:` and `ready_signal`, startup is complete; do not run sleep/status/wait loops afterward unless diagnosing a failure.
- For real workers, `quick-start` requires a current tmux leader pane. If it says the leader must run inside tmux, restart the leader with `team-agent codex`/`team-agent claude` or use an existing tmux-managed layout, then run quick-start again.
- Quick-start generated files stay inside the selected team directory, for example `.team/current/` or `.team/alpha/`; do not create or expect root `team.spec.yaml` or `team_state.md`.
- Use `team-agent quick-start ./roles --team-id alpha` to create a second generated team under `.team/alpha/`, or pass an existing team directory directly such as `team-agent quick-start .team/alpha`.
- `quick-start` is only for first-time team creation from role docs. If that team already has runtime state, use `team-agent restart . --team <session_name_or_team_name>` to resume it. If restart cannot recover context, explain the loss and wait for explicit user consent before using `team-agent restart . --allow-fresh`; never reset context through quick-start.
- If the user explicitly asks a worker to create or operate a nested child team, first read `references/team-in-team.md`. Child teams must use an independent child workspace, never the parent `.team/current`.
- `team-agent send coder "Do the bounded task"` persists a message for positional logical TO and returns. Prefer this over `--watch-result`.
- Positional `TO` has two co-equal forms: an in-team short name, for example `team-agent send reviewer "Review this change"`, and a fully-qualified logical name, `<workspace>::<team>/<agent>`. Use the fully-qualified form across workspaces or when the local team scope is ambiguous.
- Advanced orchestration callers may add `--presentation-sink leader|casefile|silent --message-class CLASS [--case-id CASE]`. All sinks remain durable and pullable; `casefile`/`silent` suppress only live leader injection. Missing presentation metadata preserves the normal leader-visible behavior.
- After `send` succeeds, do not run `sleep`, `status`, `inbox`, or `collect` polling loops unless the user explicitly asks for diagnosis; the coordinator will notify the leader when a result arrives.
- `team-agent send --task task_initial "Start"` still parses but `--task` is a deprecated delivery flag (same warning family as `--watch-result`).
- `team-agent status` shows team, worker health, result-store counts, `session_id`, `captured_via`, and attribution confidence. `team-agent status --json` is compact and context-safe by default; use `team-agent status --detail --json` only for raw runtime-state diagnostics.
- `team-agent status coder` shows one worker.
- `team-agent approvals [coder]` shows structured pending approval prompts without copying worker terminal pages.
- `team-agent inbox coder` shows message history only. Final results are not in inbox.
- `team-agent shutdown --workspace . --keep-logs` stops the tmux session after a final session capture attempt.
- `team-agent restart .` restarts a stopped team from stored worker sessions. If one workspace has multiple restartable teams, use `team-agent restart . --team <session_name_or_team_name>`.
- `team-agent start-agent coder --workspace .` repairs one missing worker window without interrupting other workers.
- `team-agent doctor` checks local dependencies and provider auth hints.
- `team-agent results --case CASE_ID` reads reported results for a case.
- `team-agent compile` / `team-agent validate` are hidden from top-level `--help` but `--help` on those verbs works; use them to check role docs without launching.

<!-- command-coverage:normative-start -->
### Normative command inventory

This inventory is the handbook authority for command coverage. Only the command
lines between the two `command-coverage:normative-*` markers are canonical
argv forms. Examples elsewhere in this handbook may be diagnostics, negative
probes, historical observations, or prose and are not part of the inventory.

```text
team-agent add-agent <agent> --role-file <file>
team-agent add-agent reviewer --role-file .team/current/agents/reviewer.md --workspace .
team-agent approvals
team-agent approvals <agent_id>
team-agent approvals [coder]
team-agent attach-leader
team-agent claim-leader
team-agent claude
team-agent clone-agent <source> --as <new>
team-agent codex
team-agent codex --dangerously-bypass-approvals-and-sandbox
team-agent collect
team-agent doctor
team-agent fork-agent <source> --as <new>
team-agent inbox
team-agent inbox coder
team-agent profile doctor <name> --workspace . --json
team-agent profile init <name> --auth-mode subscription --workspace .
team-agent profile init claude-default --auth-mode subscription --workspace .
team-agent profile init codex-default --auth-mode subscription --workspace .
team-agent profile init deepseek --auth-mode compatible_api --workspace .
team-agent profile show <name> --workspace . --json
team-agent profile show deepseek --workspace . --json
team-agent quick-start
team-agent quick-start ./roles --team-id alpha
team-agent quick-start .team/alpha
team-agent quick-start .team/current
team-agent quick-start <dir>
team-agent remove-agent <agent> --workspace . --confirm
team-agent reset-agent <agent> --discard-session
team-agent restart
team-agent restart .
team-agent restart . --allow-fresh
team-agent restart . --team <session_name_or_team_name>
team-agent send --task task_initial "Start"
team-agent send --watch-result
team-agent send --watch-result coder "Do the bounded task"
team-agent send TO MESSAGE
team-agent send AGENT MESSAGE
team-agent send reviewer "..."
team-agent send reviewer "Review this change"
team-agent shutdown --workspace . --keep-logs
team-agent start-agent <agent>
team-agent start-agent <agent_id> --workspace .
team-agent start-agent coder --workspace .
team-agent status
team-agent status --detail --json
team-agent status --json
team-agent status coder
```

Here `AGENT` is the in-team short-name specialization of positional `TO`, not a second command syntax or new behavior.

The exact frozen CLI root help is a complementary public-surface authority:
`team-agent --help` must expose the root verb for every canonical command that
is not otherwise documented as a compatibility or provider form. The command
coverage gate parses this output from the exact test binary; it never invokes a
provider or treats a diagnostic example as a command approval.

<!-- command-coverage:normative-end -->

## Restart Semantics

`restart` takes one workspace argument. It preserves each worker's original provider. If a verified provider session exists, the worker resumes (`codex resume <id>` or `claude --resume <id>`). Claude sessions are considered resumable only after the provider has written a project transcript for that session; a freshly opened blank Claude window is not recorded as recovered context. If the stored id is stale, the runtime first tries to repair it from verified transcript history. If a stored session cannot be verified or repaired, restart fails closed instead of silently losing context; use `team-agent restart . --allow-fresh` only when the user explicitly accepts a fresh worker context. If multiple stopped teams in the same workspace have restart context, plain `team-agent restart .` fails and lists candidates; rerun with `--team <session_name_or_team_name>`. If no prior session id exists, that worker starts fresh and the event log records `restart.fresh_spawn`. Claude resume must run from the original cwd and the same provider transcript root; Team Agent stores `spawn_cwd` and compatible-API `claude_projects_root` for that.

`restart` `--help` does not mention rereading `agents/*.md`. See **How role-doc frontmatter becomes live**.

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
dangerously_skip_permissions: false
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

To **recreate** an existing worker from an updated role file:

```bash
team-agent add-agent reviewer --role-file .team/current/agents/reviewer.md --force --workspace .
```

Missing `--role-file` observed (exit 1): `usage error: missing --role-file` plus a structured `action`.

Semantic distinction:

- `team-agent add-agent <agent> --role-file <file>` — add a **new** worker not yet in team state.
- `team-agent add-agent <agent> --role-file <file> --force` — force-recreate that worker from the role file.
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

For typed orchestration traffic, both `send_message` and `report_result` accept `presentation={"sink":"leader|casefile|silent","class":"message|progress|stage_result|stage_pass|bounce|blocking|final_review|timeout","case_id":"optional-case"}`. If the object is present, `sink` and `class` are required and unknown values fail closed. `casefile` and `silent` are durable-only, not deletion. The fixed critical classes `stage_pass`, `bounce`, `blocking`, `final_review`, and `timeout` always appear on the leader screen even when another sink is requested. Routing uses the typed class, never words in the content or summary.

Do not pass `sender`, `task_id`, `requires_ack`, `schema_version`, or `agent_id` unless doing a low-level compatibility diagnostic. The MCP runtime fills those fields and keeps delivery metadata in runtime state and event logs. If provider env loses the worker id, MCP infers it from active task/message state and falls back to an explicit `unknown` sender instead of treating the worker as leader.

Message targets are team-scoped. Use `leader`, another teammate agent id, or `*` for all other team members. The runtime excludes the sender from `*` broadcasts and never scans unrelated terminal windows for recipients.

`report_result` stores final completion and immediately attempts a leader notification through the verified/fallback delivery path. `team-agent collect` remains the authoritative state-update path. Do not wait for final results through `team-agent inbox`, message ack counts, or repeated plain status polling. `acknowledged_count` only means prior task messages were acknowledged by the worker; it is not a missing-result signal.

For normal leader dispatch, prefer positional `team-agent send <TO> "..."` and the returned message id; do not use `--watch-result`.

For long processes, workers must write logs, keep a pid, provide a health check, and stop after a bounded number of retries. QA/reviewer roles must stay within their authorized files and stop on service unavailable, approval prompts, or repeated startup failure.

## Failure Rules

For any non-zero `team-agent` exit, report the command, exit code, last about 20 stderr lines, and affected task or agent when known.

**If the error JSON/text includes a structured `action` field, run that `action` first.** This rule does not expire with skill versions.

Reproduce a structured `action` (missing `dangerously_skip_permissions` → compile JSON):

```bash
d=$(mktemp -d) && mkdir -p "$d/agents"
printf '%s\n' '---' 'name: t' 'objective: t' 'provider: fake' '---' 'x' > "$d/TEAM.md"
printf '%s\n' '---' 'name: coder' 'role: Worker' 'provider: fake' 'model: fake' 'auth_mode: subscription' 'tools:' '  - mcp_team' '---' 'b' > "$d/agents/coder.md"
team-agent compile --team "$d" --out /tmp/ta-failure-rules-compile.yaml --json
```

Observed (exit 1): JSON includes `"ok": false` and `"action": "run \`team-agent doctor\` or inspect the log path shown here"`, plus `missing front matter field dangerously_skip_permissions`. Prefer the `action` over guessing flags.

`coordinator.session_missing` is a self-healing transient: run `team-agent status --json`, if `ready` is true (or workers show `running`) continue; do not shutdown and wait.

`result_success_without_executed_tests` on a **non-coding** task (plain hello / status ping) is an advisory you may ignore. It is not a failed delivery.

Examples of `action` observed on this gauge:

- compile/add-agent usage failure: `"action": "run \`team-agent doctor\` or inspect the log path shown here"`
- send with no runtime: `"action": "Run team-agent quick-start/restart in the target workspace, or choose a workspace that has .team/runtime/state.json"`
- `claim-leader` from a worker pane: `"action": "pane %21 is registered as worker developer-d108; run claim-leader from the leader's own pane, not a worker pane"` (exit 1, `ok: false`, `reason: caller_not_leader_shaped`)

Do not retry with changed flags. Do not inspect source code or private runtime state. Do not operate tmux directly except when the user asks for a manual diagnostic. Do not answer provider approval prompts for the user.

If `quick-start` reports `tmux session already exists`, treat it as a team-name collision. The existing session may be an active team; do not terminate it and do not suggest `shutdown` as the normal fix. Change `name:` in `TEAM.md` so the next launch uses a different tmux session name, then run `team-agent quick-start .team/current` again.

Known Team Agent control-plane MCP prompts such as `team_orchestrator.report_result` and `team_orchestrator.send_message` are handled by the coordinator. It uses session-scoped approval, verifies the prompt cleared, retries boundedly, and logs the result. Do not ask the user to approve those routine internal prompts.

When `status` still shows `AWAITING_APPROVAL`, run `team-agent approvals <agent_id>`, show the structured prompt summary and choices, ask the user to decide, and wait.

Do not inspect raw worker terminal output during normal operation. Use `team-agent status`, `team-agent approvals`, `team-agent inbox`, `team-agent collect`, and event logs instead. Raw-screen diagnostics are outside this skill's normal workflow, require explicit user authorization, and are guarded by the CLI; use them only as a one-shot bounded diagnostic, never as a routine workflow step.

Then stop and wait for the user **after** you have executed a provided `action` (or reported that it cannot be run from this pane).

For "worker reported but leader cannot see completion":

1. Run `team-agent collect` once; this is the final-result intake path.
2. If no result is collected, inspect `team-agent status --json` field `results`. `uncollected > 0` means the result is already accepted by MCP and waiting in the result store.
3. Check `.team/logs/events.jsonl` for `mcp.report_result` and `collect.result` before sending another prompt to the worker.
4. Do not loop on `team-agent inbox` or ack/status counts; that burns context and cannot consume final results.
