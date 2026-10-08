# Provider Adaptation Contract & Specification

Status: **proposed v1** (design contract; no runtime behavior is changed by this document).
Baseline: anchored to `main` at `8ff50cfa` (golden baseline 2, version `0.5.112`).
Audience: engineers adding a new provider CLI, or changing one facet of an existing provider.

All source anchors are relative to `crates/team-agent/src/` and name a symbol rather than a line,
because line numbers drift. "Current" always means the anchored baseline.

---

## 0. How to read this contract

### 0.1 Normative keywords

**MUST / MUST NOT / SHOULD / MAY** follow RFC 2119. A rule marked *(target)* describes the contract
shape a new provider is written against; where today's code differs, the difference is listed in the
**Deviation Register (§13)**. A deviation entry is a record, **not** an authorization to change
existing behavior. Every migration of an existing provider is a separately scoped, separately
accepted change.

### 0.2 Evidence ladder for every provider claim

Every facet of every provider carries exactly one evidence level. Levels never promote implicitly.

| Level | Meaning | Typical proof | Does NOT prove |
|---|---|---|---|
| **Declared** | A descriptor, enum, flag table or comment says so | source text | that any caller reaches it |
| **Reachable** | A production caller on a public path invokes it | call graph from a public CLI/MCP entry | that the native CLI honors it |
| **Observed** | A real run produced the expected native artifact/screen | live capture, transcript, event log | that the business operation succeeded |
| **Verified** | An end-to-end acceptance case passed with recorded receipts | acceptance receipt + logs | other models, auth modes, platforms, or operations |

Examples from the baseline: `ProviderCaps.fork = true` for Codex is *Declared*, while public
`fork-agent` for Codex refuses with `codex in-window fork is unverified (未验证)`; Pi
`recipient_requires_single_enter` is *Declared* (definition + tests) but not *Reachable*.

### 0.3 Ten laws (summary of the whole contract)

1. **Provider ≠ topology.** Host shell policy is chosen by the *operation* (cold, dynamic, leader), never by the provider.
2. **Facets, not booleans.** Each capability is a typed *mode* plus an evidence level. `false` never stands in for "unknown".
3. **Data in the descriptor, behavior in narrow hooks.** If two providers differ only by a literal (flag, mark, binary, delay), it is descriptor data. A hook exists only where I/O or parsing is required.
4. **Plan is pure; Materialize owns I/O and returns a receipt; Spawn belongs to the Transport.**
5. **Two communication axes.** Outbound (agent → team) is always an MCP tool call. Inbound (team → pane) is keystrokes chosen by *(channel, recipient input profile)*.
6. **Keystroke policy is an explicit parameter**, never ambient thread-local state and never inferred from a provider name inside the Transport.
7. **Evidence never promotes upward.** accepted ≠ pasted ≠ submitted ≠ consumed ≠ turn started ≠ tools verified ≠ natural return ≠ business success.
8. **Unknown is first-class.** It never collapses into false / idle / ready / dead.
9. **Fork is a typed family.** Each kind has its own identity result, evidence and rollback semantics.
10. **Teardown acts only on receipts.** Nothing is killed or deleted by provider name, process name or bare cwd; provider backing is preserved by default; global writes require a reversible receipt.

A corollary, learned from partial integrations: **honest partiality**. A provider that does not carry
an input (for example a system prompt) MUST declare that carrier as `None`, and the lifecycle MUST
surface it; a builder MUST NOT silently drop an input it was given.

---

## 1. Vocabulary

| Term | Definition |
|---|---|
| Provider | A native agent CLI driven inside a terminal pane (Codex, Claude/ClaudeCode, Pi, CursorAgent, Grok, GeminiCli; Copilot is a comparison provider; Fake is test-only). |
| Wire name | Canonical persisted string (`provider/wire.rs::provider_wire`). Immutable once shipped. |
| Alias | Historical parse-only string (`provider/wire.rs::aliases`). Never written back. |
| Binary | Executable name (`provider/wire.rs::command_name`). Distinct from wire (`claude_code` → `claude`, `cursor_agent` → `agent`, `gemini_cli` → `gemini`). |
| Seat | One agent id inside one team, bound to one pane on one tmux endpoint for one spawn epoch. |
| Operation | cold start, dynamic start/add/restart, managed leader, fork target, shutdown, … |
| Channel | The caller-side path that types into a pane: worker delivery, human leader, leader fallback, app-server, in-place fork command, startup menu, empty wake, fast-mode toggle. |
| Facet | One independently verifiable capability: model, effort, auth, bypass, prompt, MCP, session, fork, input, startup, probes, workspace, teardown. |
| Receipt | Durable proof written by the component that performed an effect (materialization receipt, delivery receipt, fork receipt, shutdown classification). |
| Token | `[team-agent-token:<message_id>]`; message-scoped correlation marker, never a credential or session id. |
| Natural return | An MCP `send_message`/`report_result` call made by the agent itself under its captured identity. |

---

## 2. Provider integration surface

The contract splits a provider into one **static descriptor** and a small set of **optional hooks**.
Today all six providers share one `BasicProviderAdapter` (`provider/adapter.rs::get_adapter`) with
per-provider `match` arms, free functions in `provider/adapters/*.rs`, and `if provider == X` blocks in
lifecycle code. The descriptor/hook split is the *(target)* shape; §5 maps every slot to today's code.

### 2.1 `ProviderDescriptor` (static data, no I/O)

```rust
// Illustrative contract shape — not compiled code.
pub struct ProviderDescriptor {
    pub identity:  Identity,        // wire, aliases, binary, public leader verb
    pub maturity:  Maturity,        // L0..L5, see §11
    pub model:     ModelPolicy,     // selector shape + trim + catalog membership
    pub effort:    EffortPolicy,    // 6-literal admission table + carrier + inheritance
    pub auth:      AuthPolicy,      // accepted AuthModes + credential mapper id
    pub bypass:    BypassPolicy,    // argv for dangerously_skip_permissions true / false
    pub prompt:    PromptCarrier,   // how the system prompt reaches the native CLI
    pub mcp:       McpCarrier,      // how the Team MCP server reaches the native CLI
    pub tools:     ToolNaming,      // how tool names are spelled in prompts
    pub session:   SessionPolicy,   // fresh id, resume key, backing kind, semantic reader
    pub fork:      ForkPolicy,      // (AuthMode) -> ForkKind, see §9
    pub input:     InputProfile,    // keystroke contract, see §7
    pub startup:   StartupPolicy,   // startup modals and their key actions
    pub probes:    ProbeSources,    // which Tier 1/2/3 evidence sources exist, see §8
    pub workspace: WorkspacePolicy, // cwd/config exclusivity between seats
    pub teardown:  TeardownPolicy,  // owned vs preserved vs forbidden artifacts, see §10
}

pub struct Identity {
    pub wire: &'static str,
    pub aliases: &'static [&'static str],
    pub binary: &'static str,
    pub leader_verb: Option<&'static str>,   // None => no public `team-agent <verb>`
}

pub enum ModelPolicy {
    Passthrough { trim: Trim },                         // raw string to --model
    QualifiedExact { live_catalog_membership: bool },   // provider/model, byte-exact
}

pub struct EffortPolicy {
    pub admission: [EffortAdmission; 6],  // low, medium, high, xhigh, max, ultra
    pub carrier: EffortCarrier,           // Flag("--effort") | ConfigKv(..) | Flag("--thinking") | None
    pub inherit_team_default: bool,
}
pub enum EffortAdmission { Pass, IgnoreWithEvent, Reject(&'static str) }

pub enum PromptCarrier {
    AppendFlag(&'static str),        // e.g. --append-system-prompt
    RulesFlag(&'static str),         // e.g. --rules
    FileFlag(&'static str),          // e.g. --system-file <path> (needs Materialize)
    ConfigOverride(&'static str),    // e.g. -c developer_instructions=...
    WorkspaceRulesFile(&'static str),// e.g. .cursor/rules/*.mdc (needs Materialize)
    RuntimeExtension,                // provider-side extension injects it
    None,                            // MUST be surfaced, never silently dropped
}

pub enum McpCarrier {
    InlineOrFileFlag(&'static str),                       // --mcp-config <json|file>
    ConfigFlags(&'static str),                            // -c mcp_servers.<..>=..
    DirScopedOverlay { file: &'static str, exclusive_cwd: bool },
    ProjectOverlayWithEnable { file: &'static str },      // overlay + native enable step
    RuntimeExtension,                                     // provider extension registers it
    GlobalInstaller,                                      // writes user HOME; see §10.3
    None,
}
```

Rules:

- D-1 The descriptor MUST be total: every field is set for every provider, including `None`/`Reject`/`Unsupported` values with a reason. There is no "default to the generic provider" for facts.
- D-2 Descriptor values are **literal contract data**. Flag spellings, marks, delays and error inner texts in the descriptor are byte-stable and covered by golden tests.
- D-3 A descriptor MUST NOT encode evidence it does not have. "The native CLI probably supports X" is recorded as `Unsupported { reason: Unverified }`, not as a supported mode.

### 2.2 Hooks (behavior that needs I/O or parsing)

| Hook | Required? | Signature (target) | Returns on "not applicable" |
|---|---|---|---|
| `CatalogHook` | optional | `discover(&Runner) -> Result<Vec<ModelRecord>, CatalogError>` | `CatalogError::UnsupportedProvider` |
| `PlanHook` | **required** | `plan(&LaunchRequest) -> Result<LaunchPlan, PlanError>` (pure) | — |
| `MaterializeHook` | optional | `materialize(&LaunchPlan, &SeatPaths) -> Result<MaterializationReceipt, _>` | empty receipt |
| `SessionHook` | required if `session.resume != None` | `capture(..)`, `probe_backing(..) -> BackingEvidence`, `resume_plan(..)` | `ResumeKey::None` |
| `SemanticReader` | optional | `classify(tail) -> TurnObservation`, `latest_fault(tail) -> Option<FaultFact>` | `Unsupported` |
| `StartupHook` | optional | `observe(screen) -> StartupScreen`, `action(StartupScreen) -> Vec<Key>` | `NoStartupModals` (declared, not "ready") |
| `ForkHook` | required if any `ForkKind::NewSeatSnapshot`/`NativeArgv` | `stage(..) -> ForkStaging`, `compensate(..)` | — |

Rules:

- H-1 An absent optional hook returns a **typed Unsupported**, never `Ok(true)`, an empty list, or a generic fallback. (Baseline counter-examples: `validate_model` returns `Ok(true)` for non-Pi; `build_command_plan` `_ =>` arm returns `argv_only` that drops prompt/MCP/bypass/effort; see §13.)
- H-2 `PlanHook::plan` MUST be pure: no filesystem, no subprocess, no clock, no random. Session ids come from an injected `SessionIdSource` so goldens are deterministic.
- H-3 Hooks never call tmux, never send keys, and never read credentials. Keys are produced as data (`Vec<Key>`) and sent by the caller through the Transport.

---

## 3. Standard provider lifecycle pipeline

```text
 S0 Resolve identity ─► S1 Discover catalog (on demand) ─► S2 Admit (model / effort / auth)
   ─► S3 Resolve profile + env ─► S4 Compose prompt + MCP payload ─► S5 Plan (pure)
   ─► S6 Materialize (owned I/O, receipt) ─► S7 Host-wrap (operation policy) + argv-route
   ─► S8 Spawn + pane identity ─► S9 Probe Tier1 ─► Tier2 ─► Tier3
   ─► S10 Communicate (inbound keys / outbound MCP) ─► S11 Session ops (capture / resume / fork)
   ─► S12 Teardown (receipts only)
```

| Stage | Purpose | Provider slot | Purity | Failure semantics | Current anchor |
|---|---|---|---|---|---|
| S0 Identity | wire ↔ enum ↔ binary ↔ leader verb | `identity` | pure | unknown wire is an error at canonical surfaces; aliases only on legacy reads | `provider/wire.rs::{provider_wire, parse_provider, parse_canonical_provider, command_name}`; `model/enums.rs::Provider` |
| S1 Catalog | native model list → `ModelRecord` | `CatalogHook` | I/O, bounded (10 s, 1 MiB) | no partial/static fallback; unsupported ≠ empty match | `provider/model_catalog.rs::{discover_model_catalog, parse_*_catalog, model_matches}` |
| S2 Admit | model shape, effort 3-state, auth | `model`, `effort`, `auth` | pure (except exact membership, which needs S1) | reject with byte-stable inner text; **no auto-downgrade** | `model/enums.rs::ProviderEffort::resolve_for_provider`; `compiler.rs::{compile_role_agent_with_mode, validate_pi_role_fields}`; `model/spec.rs::validate_spec`; `lifecycle/launch/pi_mcp.rs::select_exact_pi_model` |
| S3 Profile + env | profile exports/unsets, identity isolation, caller context | `auth` (credential mapper) | pure map building; file read of profile | `unset` wins over overlay; never log values | `lifecycle/profile_launch.rs::{prepare_provider_profile_launch_with_profile_dir, provider_env_exports, provider_env_unsets, provider_command_overrides}`; `layout/worker_env.rs` |
| S4 Compose | system prompt order, MCP server config | `prompt`, `mcp`, `tools` | prompt file read only | prompt order fixed: identity → runtime MCP contract → communication → body → output contract | `lifecycle/worker_command_context.rs::compile_worker_system_prompt`; `provider/adapter.rs::mcp_server_config`; `lifecycle/launch/mcp_config.rs::resolve_mcp_config` |
| S5 Plan | argv + expected session + materialization requests | `PlanHook` | **pure** | typed `PlanError`; `CarrierReport` lists consumed and not-carried inputs | `provider/adapter.rs::{build_command_plan, build_resume_command_plan, fork_plan}`; `provider/adapters/*.rs` |
| S6 Materialize | seat-owned files, overlays, extensions, session roots | `MaterializeHook`, `workspace` | I/O, owned paths only | idempotent; receipt lists created vs pre-existing paths | Pi: `lifecycle/launch/pi_mcp.rs::{materialize_pi_plan, materialize_pi_resume_plan}`; Cursor: `lifecycle/launch/cursor_mcp.rs::{prepare_cursor_seat_mcp, apply_cursor_spawn_workspace_pointers}`; Grok: `lifecycle/launch/mcp_config.rs::{ensure_grok_login_and_folder_trust, apply_grok_mcp_overlay}`; generic: `write_worker_mcp_config_for_provider`, `point_native_mcp_config_at_file` |
| S7 Host-wrap | shell line by operation; host argv-route | none (operation-owned) | pure string building | 16,000-byte envelope limit, reject before spawn | `tmux_backend.rs::{shell_command, worker_shell_wrapper_command, leader_shell_wrapper_command, validate_spawn_command}`; `provider/argv_route.rs` |
| S8 Spawn | create pane, verify returned pane identity | none | Transport I/O | 500 ms / 25 ms identity check; mis-bound pane is killed (own pane only) | `tmux_backend.rs::{spawn, spawn_with_command, spawn_split}`; `lifecycle/launch/spawn.rs::spawn_agents`; `lifecycle/restart/common.rs::spawn_agent_window` |
| S9 Probe | Tier 1 → Tier 2 → Tier 3 (§8) | `probes`, `startup` | observation I/O | typed tri-state; timeout ≠ dead | §8 |
| S10 Communicate | inbound delivery, outbound MCP | `input`, `tools` | Transport I/O | evidence ladder (§6.3); exactly-once guard after physical submit | `messaging/delivery.rs::{deliver_pending_messages, deliver_prepared_message}`; `tmux_backend.rs::inject_with_submit_observer`; `mcp_server/wire.rs::dispatch_tool` |
| S11 Session ops | capture, backing probe, resume, fork | `session`, `fork`, `SessionHook`, `ForkHook` | I/O | resume never silently degrades to fresh | `provider/session_scan/*`; `lifecycle/restart/common.rs::resume_backing_probe_for_agent`; `lifecycle/restart/selection.rs::decide_start_mode`; §9 |
| S12 Teardown | stop / reset / remove / scoped shutdown | `teardown` | destructive I/O on receipts | ordered effects; partial/dirty results preserved | `lifecycle/restart/agent.rs::{stop_agent_at_paths, reset_agent_at_paths}`; `lifecycle/restart/remove.rs::remove_agent_inner`; `cli/mod.rs::lifecycle_port::shutdown_with_transport_and_state` |

Pipeline rules:

- P-1 **One entry per stage.** Cold start (`spawn_agents`) and dynamic start (`spawn_agent_window`) MUST call the same S5/S6 functions. Provider-specific materialization MUST NOT be duplicated per entry point.
- P-2 **Stage outputs are typed and carried forward**; a later stage MUST NOT re-derive an earlier fact from strings (for example, re-parsing the token out of rendered text, §7.2).
- P-3 **Native leader passthrough is a different pipeline.** Non-Pi leader verbs pass raw user argv to the native CLI (S2 does not run); Pi leader accepts only one `--model` and one `--thinking` and then runs S2/S6. Worker admission tables MUST NOT be applied to leader passthrough.
- P-4 **S7 is operation-owned.** `direct` (cold, display split), `worker` (dynamic, fork target; provider runs as a child, inert tail on exit), `leader` (managed leader; numeric exit receipt in a pane option). A provider MUST NOT choose its shell policy.
- P-5 **Host argv-route is spliced exactly once**, after the executable, at the final launch site (cold, dynamic, native leader). Auxiliary commands (version, catalog, auth, MCP enable) are not routed.

---

## 4. Admission, argv and environment contract (S2–S5)

### 4.1 Model

| Policy | Providers (current) | Rules |
|---|---|---|
| `Passthrough { trim: None }` | Codex, Claude/ClaudeCode, CursorAgent, GeminiCli | raw non-blank role string → `--model RAW`; no lowercasing, no catalog membership, no effort derived from suffixes such as `-high` or `:fast` |
| `Passthrough { trim: Both }` | Grok | outer whitespace trimmed at argv build; inner preserved |
| `QualifiedExact { live_catalog_membership: true }` | Pi | `raw == trim`, non-empty, no `*`, `split_once('/')` both sides non-empty; exactly one byte-exact catalog member; extra slashes and inner spaces are legal |

- M-1 An omitted or blank role model MUST NOT be filled from TEAM `default_model`/`provider_models`. Native defaults apply unless a profile `MODEL` override exists (`provider_command_overrides`).
- M-2 A new provider SHOULD start with `Passthrough { trim: None }` unless the native CLI has an exact catalog contract that can be observed at launch.
- M-3 Pi preflight candidate suggestion (last-slash right segment, case-sensitive exact) is advisory and always still rejects; `model/name_similarity.rs` MUST NOT be used for model selection.

### 4.2 Effort (three-state admission, six literals)

`S` = pass, `I` = ignore with `provider.effort_unsupported` event, `E` = reject.

| Provider | low | medium | high | xhigh | max | ultra | Carrier | Inherit TEAM |
|---|---|---|---|---|---|---|---|---|
| Codex | S | S | S | S | S | S | `-c model_reasoning_effort=LEVEL` (after profile config) | yes |
| Claude/ClaudeCode | S | S | S | S | S | E | `--effort LEVEL` | yes |
| Pi | S | S | S | S | S | E | `--thinking LEVEL` | **no** |
| CursorAgent | E | E | E | E | E | E | none | yes (then rejected) |
| Grok | S | S | S | S | E | E | `--effort LEVEL` | yes |
| GeminiCli | I | I | I | I | E | E | none | yes (then ignored) |

- E-1 There is **no automatic downgrade** (`max → xhigh`, `ultra → max`, …). Codex `ultra` stays literal (client delegation).
- E-2 Parsing trims but is case-sensitive. Role value beats TEAM value. When neither is set, no flag is emitted.
- E-3 Inner error texts are contract bytes: `cursor_agent does not support effort; the Cursor CLI has no --effort flag`, `effort 'ultra' is only supported by codex`, `effort 'max' is only supported by claude/claude_code/codex/pi`. Each entry (CLI, compiler, spec, runtime) keeps its own wrapper.
- E-4 A new provider declares its row explicitly. Whether a given *model* honors a level is `U` unless the catalog exposes it; `S` proves policy + argv only.

### 4.3 Bypass, auth, profile, env

| Provider | `dangerously_skip_permissions = true` | `false` | Credential mapper |
|---|---|---|---|
| Codex | `--dangerously-bypass-approvals-and-sandbox` | (none) | OpenAI / custom provider env key |
| Claude/ClaudeCode | `--dangerously-skip-permissions` | `--permission-mode default` (explicit) | Anthropic official vs compatible; isolated config dir |
| Pi | (no flag) | (no flag) | none (native subscription + extension) |
| CursorAgent | `--trust --sandbox disabled --force` | (none) | none (proxy pass-through only) |
| Grok | `--always-approve` | (none) | none (login/folder trust gate) |
| GeminiCli | **not carried** | not carried | `GEMINI_API_KEY` export, unset under compatible |

- A-1 A provider whose bypass is not carried MUST declare `BypassPolicy::NotCarried`; the lifecycle SHOULD warn when a role requests bypass for it *(target)*.
- A-2 Env order is fixed: inherited parent env → strip identity/TMUX/caller → worker overlay → profile `unset` then `extend` → approval/provider overlay → final isolation → `env_unset` handed to the transport. **Unset wins**; a key exported and unset is absent in the child.
- A-3 Caller context (`TMUX`, `TMUX_PANE`) is bound inside the target pane's real shell, never copied from the parent caller snapshot.
- A-4 Secret values are never read into logs, receipts, prompts or argv-route tokens. The common MCP server entry is the **current Team Agent executable absolute path** with `mcp-server --workspace {workspace}` and identity env only.

### 4.4 Plan output *(target)*

```rust
pub struct LaunchPlan {
    pub argv: Vec<String>,                       // ordered vector, never a shell string
    pub expected_session: Option<SessionId>,     // preassigned id, if the CLI accepts one
    pub provider_projects_root: Option<PathBuf>,
    pub materialization: Vec<MaterializationRequest>,
    pub carriers: CarrierReport,                 // which inputs were carried / not carried
}
pub struct CarrierReport { pub prompt: Carried, pub mcp: Carried, pub bypass: Carried, pub effort: Carried }
pub enum Carried { Yes, NotRequested, NotCarried { reason: &'static str } }
```

Current anchor: `provider/types.rs::CommandPlan { argv, expected_session_id, provider_projects_root, managed_mcp_config }`.
`CarrierReport` does not exist today (see §13, D-08).

---

## 5. Behavior-to-function map

"I want provider X to do Y" → which slot to fill → where it lives today.

### 5.1 Identity, models, effort

| Behavior | Contract slot | Current anchor (baseline) |
|---|---|---|
| Register a new provider id | `identity` | `model/enums.rs::Provider` (exhaustive enum); `provider/wire.rs::{provider_wire, aliases, command_name, ALL_PROVIDERS}` |
| Accept a historical spelling | `identity.aliases` | `provider/wire.rs::aliases` (canonical first) |
| Expose `team-agent <verb>` for a native leader | `identity.leader_verb` | `cli/leader.rs::leader_passthrough_provider`; `cli/emit.rs`; `cli/spec.rs`; `leader/start.rs::provider_command_argv` |
| List native models | `CatalogHook` | `provider/model_catalog.rs::{catalog_source, discover_model_catalog, parse_*_catalog}`; follow `docs/reference/provider-model-discovery.md` |
| Validate model shape | `model` | Pi: `compiler.rs::validate_pi_role_fields`, `lifecycle/launch/pi_mcp.rs::select_exact_pi_model`; others: passthrough |
| Map effort literal to argv | `effort` | `model/enums.rs::ProviderEffort::{is_supported_by, resolve_for_provider}`; carriers in `provider/adapters/{codex,claude,pi,grok}.rs` |
| Emit "effort ignored" event | `effort.admission = IgnoreWithEvent` | `lifecycle/launch/identity.rs::{provider_effort_for_spawn, provider_effort_event_payload}` |

### 5.2 Argv, prompt, MCP, env, materialization

| Behavior | Contract slot | Current anchor |
|---|---|---|
| Fresh launch argv | `PlanHook` | `provider/adapter.rs::BasicProviderAdapter::build_command_plan` → `provider/adapters/*::*_base_command` |
| Resume argv | `SessionHook::resume_plan` | `provider/adapter.rs::build_resume_command_plan`; Pi: `materialize_pi_resume_plan` |
| Bypass flags | `bypass` | `provider/bypass_flags.rs`; per-adapter base commands |
| System prompt carrier | `prompt` | Claude `--append-system-prompt`; Grok `--rules`; Codex developer `-c`; Cursor rules file; Pi `--append-system-prompt` + extension |
| Tool-name spelling in prompt | `tools` | `lifecycle/worker_command_context.rs::compile_worker_system_prompt`; Pi wire binding in `lifecycle/launch/pi_mcp_extension.js` (`before_agent_start`) |
| MCP server config | `mcp` | `provider/adapter.rs::{mcp_config, mcp_server_config}`; `lifecycle/launch/mcp_config.rs::{resolve_mcp_config, write_worker_mcp_config_for_provider, point_native_mcp_config_at_file}` |
| Per-seat overlay / extension | `MaterializeHook` | Cursor `lifecycle/launch/cursor_mcp.rs`; Grok `lifecycle/launch/mcp_config.rs::apply_grok_mcp_overlay`; Pi `lifecycle/launch/pi_mcp.rs::write_pi_wrapper` |
| Cwd exclusivity between seats | `workspace` | Cursor `lifecycle/launch/cursor_mcp.rs::refuse_second_cursor_occupant`; Grok `lifecycle/launch/mcp_config.rs::grok_shared_cwd_error` |
| Credential env mapping | `auth` | `lifecycle/profile_launch.rs::{provider_env_exports, provider_env_unsets}` |
| Global HOME install | `mcp = GlobalInstaller` | `provider/adapter.rs::install_mcp` (Gemini; not reachable from normal spawn) |

### 5.3 Host, input, probes

| Behavior | Contract slot | Current anchor |
|---|---|---|
| Shell wrapper per operation | none (operation) | `tmux_backend.rs::{shell_command, worker_shell_wrapper_command, leader_shell_wrapper_command}` |
| Paste text into a pane | Transport | `transport.rs::tmux_inject_text_argv`; `tmux_backend.rs::buffer_name_for_text` |
| Submit key spelling | `input.submit` | `transport.rs::{tmux_key_name, tmux_submit_key_name, tmux_send_submit_argv}` (logical `Enter`) |
| Paste → submit delay | `input.paste_to_submit_floor` | `messaging/delivery.rs::paste_to_submit_floor_for_recipient` → `tmux_backend.rs::with_paste_to_submit_floor` (TLS) |
| Don't interrupt an active turn | `input.resubmit = NoInterrupt` | `tmux_backend.rs::{with_cursor_single_enter, should_resubmit_enter_cursor}` (TLS, Cursor only) |
| Retry Enter when not consumed | `input.resubmit = Generic` | `tmux_backend.rs::{should_resubmit_enter, should_resend_unverified_wrap_gap}` |
| Flush a native "send now" queue | `input.mark_actions` | `provider/submit_now.rs::flush_explicit_queue`; called in `messaging/delivery.rs::deliver_prepared_message` |
| Recognize the paste placeholder | `input.paste_identity` | `tmux_backend.rs::{latch_paste, PasteLatch, pasted_prompt_in_composer, paste_ready_for_enter}` |
| Dismiss startup trust/update menus | `startup` + `StartupHook` | `provider/startup_prompt.rs::{codex_,claude_,copilot_}handle_startup_prompts`; dispatch `provider/adapter.rs::handle_startup_prompts_outcome` |
| Hold messages while a modal is open | `startup` | `messaging/delivery.rs::recipient_pane_has_actionable_startup_prompt` → `queued_until_trust` |
| Codex fast-mode toggle | `startup` / channel `FastModeToggle` | `provider/adapter.rs::enable_fast_mode` (cold spawn only: `lifecycle/launch/spawn.rs`) |
| Tier 1 process liveness | `probes.process` | `tmux_backend.rs::{liveness, has_pane, provider_exit_status}`; `coordinator/steps/abnormal.rs::agent_process_liveness`; `os_probe.rs` |
| Tier 2 pane readiness | `probes.pane` | startup handlers; `lifecycle/restart/rebuild.rs::{wait_restart_readiness_or_timeout, restart_readiness}` |
| Tier 3 protocol readiness | `probes.protocol` | Pi `lifecycle/launch/pi_mcp_extension.js` stages; server marker `mcp_server/wire.rs` (`mcp.tools_list_response_written`, no consumer) |
| Turn / fault semantics | `SemanticReader` | `provider/classify.rs::{classify, latest_explicit_error_fact}` (Claude/Codex JSONL); generic `messaging/activity.rs::classify_agent_activity` |

### 5.4 Messaging, session, fork, teardown

| Behavior | Contract slot | Current anchor |
|---|---|---|
| Outbound tool call → durable row | none (Team protocol) | `mcp_server/wire.rs::dispatch_tool`; `mcp_server/tools.rs::{send_message_with_presentation, report_result_with_presentation}` |
| Render inbound envelope | none (Team protocol) | `messaging/delivery.rs::render_message` |
| Duplicate suppression | none | `messaging/delivery.rs::token_already_visible` |
| Leader receipt | channel `LeaderHuman` | `messaging/delivery.rs::observe_leader_receipt` |
| Keyless leader channel | channel `LeaderAppServer` | `codex_app_server.rs::{submit_to_bound_thread, turn_start}`; `messaging/delivery.rs::deliver_leader_via_app_server` |
| Capture session id after launch | `SessionHook::capture` | `provider/session_scan/{codex,claude,pi,cursor,grok,copilot}.rs`; `provider/adapter.rs::capture_session_candidates` |
| Prove resumable backing | `SessionHook::probe_backing` | `lifecycle/restart/common.rs::resume_backing_probe_for_agent` |
| In-place fork | `fork = InPlaceBranch` | `lifecycle/launch/fork_agent.rs::{in_window_fork, inject_clean_command, wait_for_screen_mark}` |
| New-seat fork | `fork = NewSeatSnapshot` | `lifecycle/restart/avatar.rs::{fork_pi_new_seat_locked, read_stable_source_snapshot, stage_pi_session, compensate_fork}` |
| Seat stop/reset/remove | `teardown` | `lifecycle/restart/agent.rs`, `lifecycle/restart/remove.rs` |
| Scoped shutdown | `teardown` | `cli/mod.rs::lifecycle_port::{shutdown_with_transport_and_state, reap_process_tree, process_matches_workspace, session_ownership, cleanup_owned_empty_endpoint}` |

---

## 6. Communication contract: two axes

### 6.1 Outbound (agent → team)

- O-1 The agent communicates with the team **only** through MCP tools (`send_message`, `report_result`, and the other `tools/list` entries). It MUST NOT type into its own pane or any other pane.
- O-2 Sender identity comes from the MCP server's captured `TEAM_AGENT_ID` / owner team env. Callers MUST NOT pass `sender`, `task_id` or `schema_version`; the framework injects them.
- O-3 Tool-name spelling is provider-specific (`tools` slot): Claude `mcp__<server>__<tool>`, Grok `<server>__<tool>`, Pi runtime-bound direct or proxy names; the logical protocol is the same.
- O-4 `send_message` returns after persistence (`accepted`/`queued`); it is not a delivery or a reply.

### 6.2 Inbound (team → pane) pipeline

```text
send_message(to, content)
 → identity / scope / presentation checks → persist message_id → accepted | queued | stored_only
 → coordinator deliver_pending: owner-team projection, target, liveness, trust, busy, duplicate gates
 → render envelope → physical delivery (Tmux keys | Codex app-server JSON-RPC)
 → submit / consumption evidence → receipt (leader channels)
 → delivered                       (still not a reply)
 → agent calls send_message / report_result (natural return)
 → durable row / result ledger → leader notification queue → leader sees it
```

### 6.3 Inbound evidence ladder

| Level | Fact | Produced by | Does NOT prove |
|---|---|---|---|
| E0 | `accepted` / `queued` | `messaging/send.rs::send_message` | anything physical |
| E1 | buffer loaded | `set-buffer` / `load-buffer` exit 0 | paste reached the composer |
| E2 | pasted, token/fold visible | paste-ready poll | composer is current, submission |
| E3 | submit key sent | `send-keys … Enter` exit 0 | the CLI consumed it (copy-mode can swallow keys) |
| E4 | consumed | token/paste identity left the composer, or busy observed | a new turn started |
| E5 | turn observed | busy heuristic / semantic reader | the agent understood or acted |
| E6 | receipt | leader transcript contains this token / app-server `turn/start` inProgress | natural return |
| E7 | natural return | agent's own MCP call recorded | business success |
| E8 | business success | reading the original result content | other cases |

- L-1 Each consumer states the level it needs. Delivery is `delivered` at E4 for workers; leader channels need E6.
- L-2 **Exactly-once after E3.** Once a physical submit happened, a later observer/state save error MUST NOT turn into a re-paste. If the token is visible in pane/transcript, the row is absorbed or degraded, never re-queued (`messaging/delivery.rs`, `CurrentTurnSubmitObserver`).
- L-3 `stored_only` / mailbox presentation is intentionally never physically injected.

---

## 7. Physical keystroke communication contract

This section is the micro-level contract for typing into a tmux pane. It is provider-agnostic by
construction: **the Transport executes a policy; it never decides one**.

### 7.1 Dispatch function signature

Current (baseline):

```rust
// transport.rs — trait Transport
fn inject_with_submit_observer(
    &self,
    target: &Target,
    payload: &InjectPayload,          // Empty | Text(String) | TextSkipConsumptionPoll(String)
    submit: Key,                       // Key::Enter for all message channels
    bracketed: bool,                   // true for message channels, false for slash commands
    observer: Option<&dyn SubmitObserver>,
) -> Result<InjectReport, TransportError>;
// Provider variance today: thread-locals set by the caller
//   tmux_backend::with_paste_to_submit_floor(Duration, ..)
//   tmux_backend::with_cursor_single_enter(bool, ..)
```

Target contract *(target)*:

```rust
pub fn deliver_envelope(
    transport: &dyn Transport,
    target: &Target,
    envelope: &Envelope,               // body bytes + typed MessageToken (out-of-band)
    policy: &SubmitPolicy,             // resolved from (Channel, InputProfile)
    observer: Option<&dyn SubmitObserver>,
) -> Result<DeliveryReport, TransportError>;

pub struct Envelope { pub body: String, pub token: Option<MessageToken> }

// Per-provider descriptor data (ProviderDescriptor::input).
pub struct InputProfile {
    pub bracketed_paste: bool,
    pub paste_identity: PasteIdentity,
    pub paste_ready: PasteReady,
    pub paste_to_submit_floor: Duration,
    pub submit: SubmitSequence,
    pub resubmit: ResubmitPolicy,
    pub mark_actions: &'static [MarkAction],
    pub trailer: Trailer,                    // §7.2 T-5
}
pub enum PasteIdentity {
    AnyKnown,                 // baseline: generic latch recognises `#N` and Grok line counts in every pane
    HashCounter,              // "[Pasted text #N" / "[Pasted content #N"
    LineCount,                // "[Pasted: N lines]"
    Placeholder(&'static str),// provider-specific fold text
    TokenOnly,
}

// Per-delivery policy = channel rules applied to the recipient's InputProfile.
pub struct SubmitPolicy {
    pub bracketed_paste: bool,
    pub paste_ready: PasteReady,             // window + poll
    pub paste_to_submit_floor: Duration,     // measured from paste completion
    pub submit: SubmitSequence,              // unconditional key sequence per submission
    pub consumption: ConsumptionProbe,       // TokenAndPasteIdentity { .. } | Skip
    pub resubmit: ResubmitPolicy,            // evidence-gated retries
    pub mark_actions: &'static [MarkAction], // evidence-gated follow-up keys (e.g. queue flush)
    pub paste_identity: PasteIdentity,
}

pub fn resolve_submit_policy(channel: Channel, recipient: Option<&InputProfile>) -> SubmitPolicy;
```

- K-1 The policy MUST be passed explicitly. Thread-local scopes are a baseline mechanism (deviation D-02) and MUST NOT be extended to new providers; every channel that bypasses the scope (for example the leader fallback pane) silently gets generic behavior today.
- K-2 The Transport MUST NOT look up the recipient provider. Provider variance enters only through `SubmitPolicy`.

### 7.2 Envelope, delimiter and trailing-token protocol

```text
Team Agent message from <sender>[ for <task_id>]:\n
\n
<content>\n
\n
[team-agent-token:<message_id>]
```

- T-1 The token marker is the **final bytes** of the payload. There is **no trailing LF** after `]`.
- T-2 Body bytes are raw: no trim, no escaping, no LF→submit conversion, no added newline. Large bodies (≥ 16 KiB) are loaded through `load-buffer -` on stdin; smaller through `set-buffer`.
- T-3 The token is passed **out-of-band** to the delivery layer *(target)*. Today the backend re-derives it with the *first* `[team-agent-token:` occurrence in the body (`tmux_backend.rs::payload_token_marker`), which can pick a token quoted inside `<content>` (deviation D-03). Higher layers (duplicate check, leader receipt) already use the real `message_id`.
- T-4 Buffer name is `team-agent-send-<token>` (fallback `team-agent-buf`). It is not a global mutex; the 200 ms pane-input lock is soft (warn and proceed).
- T-5 **Trailer slot.** `Trailer::None` is the only value in use. A provider MAY declare `Trailer::Newline` only with live evidence that its composer requires it, because a trailing LF inside a bracketed paste can itself submit or split the message in other TUIs.
- T-6 `InjectPayload::Empty` (direct submit key, no buffer) and `Text("")` are different; empty text MUST NOT be used as a wake.

### 7.3 Normative physical sequence

| Step | Action | Policy slot | Baseline values |
|---|---|---|---|
| P0 | acquire pane-input soft lock | — | 200 ms, warn and continue |
| P1 | load buffer (`set-buffer -b BUF RAW` / `load-buffer -b BUF -`) | — | threshold 16 KiB |
| P2 | `paste-buffer -t PANE -b BUF [-p]`, then `delete-buffer -b BUF` | `bracketed_paste` | `-p` for message channels; once per message, **never re-pasted** |
| P3 | wait for paste-ready: token or this paste's fold placeholder in the bottom 15 non-empty lines | `paste_ready` | Tail 80, every 50 ms, window `max(2000 ms, bytes/25 ms)`; timeout ⇒ proceed (not a failure) |
| P4 | enforce paste→submit floor from paste completion (P3 time counts) | `paste_to_submit_floor` | Cursor 1 s, Grok 1 s, others 0 |
| P5 | before **every** key: query pane mode and cancel it | — (fixed) | copy/unknown `-X cancel`; tree/view `q`; client `d`; failure does not fail submit |
| P6 | send the submit sequence | `submit` | `[Enter]` (logical `Enter`, not `C-m`) |
| P7 | consumption probe | `consumption` | 12 × 100 ms, Tail 40; ≤ 4 consecutive capture failures (20 ms pause) ⇒ re-read only, never add keys |
| P8 | evidence-gated resubmit | `resubmit` | generic: ≤ 3 total submits + ≤ 1 wrap-gap extra; Cursor: ≤ 3 total, stop on busy, require token in bottom 5 lines, no wrap-gap |
| P9 | evidence-gated mark actions | `mark_actions` | Grok `Enter:send now`: ≤ 8 Enters, 50 ms apart; disabled by `TEAM_AGENT_KEEP_PROVIDER_QUEUE` |
| P10 | report | — | see §7.6 |

Hard prohibitions (apply to every provider unless a future contract revision says otherwise with live evidence):

- X-1 MUST NOT send `Escape`, `C-c`, or a literal `ESC [ 201 ~` before or between submits. (Escape can clear a Claude composer; an orphan CSI 201 corrupts input.) The baseline still *constructs* `escape_argv` and discards it; `Key::Escape` and the CSI-201 helper exist but are not sent.
- X-2 MUST NOT type message bodies key-by-key (`send-keys -l`) and MUST NOT send one Enter per body LF.
- X-3 MUST NOT re-paste on retry. Retries are submit keys only.
- X-4 MUST NOT treat token absence as consumption if the token was never seen (`NeverSeen`), and MUST re-check this paste's identity when the token is gone (fold residue).
- X-5 MUST NOT add a key because a capture failed.

### 7.4 Enter-count extension slots

The contract separates three kinds of Enter, because they carry different risks:

| Kind | Slot | Condition | Risk it controls |
|---|---|---|---|
| **Submission keys** | `submit: SubmitSequence { keys, inter_key_delay }` | unconditional, every submission | CLIs that always need N keys to send (e.g. "Enter to confirm, Enter to send") |
| **Resubmit keys** | `resubmit: ResubmitPolicy` | only while evidence shows *this* paste still in the composer and no busy turn | lost first Enter during TUI initialisation |
| **Mark-gated keys** | `mark_actions: [MarkAction { mark, key, max_presses, interval, tail }]` | only while a literal native mark is on screen | native queues ("send now"), large-paste confirmations |

```rust
pub struct SubmitSequence { pub keys: &'static [Key], pub inter_key_delay: Duration }
pub enum ResubmitPolicy {
    Generic { max_total_submits: u8, wrap_gap_extra: u8 },                         // baseline 3, 1
    NoInterrupt { max_total_submits: u8, stop_on_busy: bool, token_window_lines: u8 }, // Cursor: 3, true, 5
    Never,
}
pub struct MarkAction { pub mark: &'static str, pub key: Key, pub max_presses: u8, pub interval: Duration, pub tail: u32 }
```

Rules:

- N-1 **Prefer mark-gated keys over unconditional sequences.** If a native UI shows a confirmation mark, declare a `MarkAction`; zero extra keys are sent when the mark is absent. Use `SubmitSequence` with more than one key only when the CLI gives no visible mark and live evidence shows a single key never sends.
- N-2 Each key of a `SubmitSequence` is preceded by P5. The consumption probe starts after the whole sequence. A resubmit sends **one** key (the last key of the sequence), never the whole sequence.
- N-3 `ResubmitPolicy::Never` is the only way to express "hard single Enter". The baseline "Cursor single enter" is `NoInterrupt`, not `Never`: it can still press up to 3 times when the token is still in the bottom 5 lines and nothing is busy.
- N-4 Mark texts are byte-literal, case-sensitive, and recorded with the CLI version they were observed on (`provider/submit_now.rs::GROK_SEND_NOW_MARK = "Enter:send now"`; Cursor's footer `enter send now` is a different string and is not flushed, because a second Enter interrupts Cursor's active turn).
- N-5 Mark actions are scoped by the recipient's `InputProfile`. *(target)* The baseline scans the Grok mark for **every** non-Cursor recipient (deviation D-06).
- N-6 **Upper bound formula** per message on the worker channel:
  `|submit.keys| × max_total_submits + wrap_gap_extra + Σ mark_actions.max_presses`.
  Baseline generic recipient: `1×3 + 1 + 8 = 12` only if the Grok mark appears; normally 1. The bound is a ceiling, not a recipe.

### 7.5 Channel matrix (policy = f(channel, recipient profile))

| Channel | Payload | bracketed | Policy source | Baseline physical behavior |
|---|---|---|---|---|
| WorkerDelivery | `Text` + token | true | recipient `InputProfile` | §7.4 (Cursor/Grok floor 1 s; Cursor NoInterrupt; Grok mark flush) |
| LeaderHuman | `TextSkipConsumptionPoll` + token | true | fixed: floor 0, single submit, no consumption poll | 1 Enter; token pre-poll; needs leader receipt (E6) |
| LeaderFallbackPane | `Text` + token | true | **not resolved today** (generic TLS defaults) | generic retries; `messaging/leader_receiver.rs::deliver_to_leader_fallback_pane` |
| LeaderAppServer | JSON-RPC `turn/start` + token + clientUserMessageId | n/a | fixed: zero keys | Unix socket only; `inProgress` ⇒ receipt, not reply |
| InPlaceForkCommand | bare slash command, no token | false | fork descriptor | initial 1 Enter + ≤ 7 screen-polled Enters (total ≤ 8), never re-pasted |
| StartupMenu | `send_keys` list, no buffer | n/a | `StartupHook::action` | Codex update `[Down, Enter]`, trust `[Enter]`; Claude trust `[Enter]` |
| EmptyWake | `Empty` | n/a | fixed | 1 submit key, no consumption check |
| FastModeToggle | `send_keys` `Char` sequence + `Enter` | n/a | descriptor (Codex `/fast`) | cold spawn only when `runtime.fast` |

- C-1 A new channel MUST be added to this table with its payload kind, bracketed flag and policy source before it types into any pane.
- C-2 Non-`Enter` submit keys skip the consumption loop entirely (single send, no consumption check).

### 7.6 Delivery report *(target)*

```rust
pub struct DeliveryReport {
    pub stage_reached: InjectStage,
    pub paste_ready: PasteReadyOutcome,          // Ready | TimedOut | NoToken | CaptureFailed
    pub submit_keys_sent: u8,                    // Enter presses, all kinds
    pub resubmits: u8,
    pub mark_presses: u8,
    pub capture_reads: u16,
    pub capture_failures: u16,
    pub submit_verification: SubmitVerification,
    pub turn_verification: TurnVerification,     // metadata only
}
```

Baseline `InjectReport.attempts = consumption_attempts + capture_retries` and is **not** an Enter
count (deviation D-04). Flush presses are logged separately as `send.grok_send_now.extra_enters`.

### 7.7 Prompt-wait and natural-response rules

Waiting for a prompt:

- W-1 Before the first delivery to a seat, Tier 2 (§8) MUST NOT be `Blocked`. A seat with an actionable startup modal keeps messages in `queued_until_trust`. A failed capture is not proof that no modal exists.
- W-2 A seat marked busy by the framework keeps batch messages queued; native "busy" screens are only consulted through mark actions.
- W-3 No provider prompt regex gates submission. `status_patterns` regexes are definitions only and are not consumed by the runtime activity classifier.
- W-4 Paste-ready is a local "can press now" signal, not readiness of the provider.

Parsing responses:

- R-1 **The only natural response is an MCP call.** Team Agent never parses a reply from pane text. Screen observations are delivery evidence (E2–E5) and startup/queue marks only.
- R-2 Semantic readers (Claude/Codex JSONL) produce turn/fault observations, not replies. Providers without a reader report `Unsupported`, never `Idle`.
- R-3 Busy signals (`working`, `thinking`, `processing`, `esc to interrupt`, spinner glyphs in the bottom 15 lines) are heuristics; they can be false positives from message text or history.
- R-4 Waiting for a result uses the durable result ledger (FIFO watchers / `wait_for_result` / collect). An early empty collect is not a failure; `accepted` is not a reply; a result with `leader_notified = false, notification_status = queued` is a success whose notification is pending.

### 7.8 Baseline input profiles

Paste identity is `AnyKnown` for every provider in the baseline (one generic latch); the column
lists the placeholder each CLI is known to render.

| Provider | floor | resubmit | mark actions | known placeholder | notes |
|---|---|---|---|---|---|
| Codex | 0 | Generic(3, 1) | Grok mark scanned (D-06) | `[Pasted text #N]` | startup trust/update handler |
| Claude/ClaudeCode | 0 | Generic(3, 1) | Grok mark scanned (D-06) | `[Pasted text #N]` / `[Pasted content #N]` | Escape forbidden (clears composer) |
| Pi | 0 | Generic(3, 1) | Grok mark scanned (D-06) | U (token used) | single-Enter / no-flush helpers exist but are not wired (D-05) |
| CursorAgent | 1 s | NoInterrupt(3, busy-stop, 5 lines) | none | U (token used) | second Enter interrupts an active turn |
| Grok | 1 s | Generic(3, 1) | `Enter:send now` ≤ 8 × 50 ms | `[Pasted: N lines]` (line-count latch) | first Enter may only enqueue when busy |
| GeminiCli | 0 | Generic(3, 1) | Grok mark scanned (D-06) | U | no Gemini-specific evidence; generic is not a verified standard |

---

## 8. Three-tier probe contract

### 8.1 Result type

```rust
pub enum Probe<E> {
    Verified(E),                          // positive evidence for THIS seat instance (spawn epoch)
    NotYet { elapsed: Duration },         // still within budget
    Negative(Reason),                     // positive evidence of the opposite (dead, blocked, failed)
    Unknown(Reason),                      // evidence missing or unreadable
    Unsupported,                          // this provider declares no source for this tier
}
```

- PR-1 Tiers are evaluated in order. A higher tier MUST NOT be evaluated as `Verified` while a lower tier is not `Verified`, and a higher-tier `Verified` MUST NOT be used to infer a lower tier.
- PR-2 `Unknown`, `Unsupported` and timeouts never collapse into `Negative`, `Idle`, `Ready` or `Dead`.
- PR-3 Every tier result is bound to the seat's spawn epoch; stale artifacts (old rollouts, old screen markers, old mtimes) MUST NOT satisfy a newer epoch.

### 8.2 Tier 1 — Process liveness

Question: *is the provider process (not the pane, not the wrapper shell) alive for this seat instance?*

| Evidence source (ordered) | Strength | Baseline anchor |
|---|---|---|
| Supervisor exit receipt (numeric pane option / complete worker rc marker) | Negative or Verified-exited | `tmux_backend.rs::provider_exit_status`; worker marker `[team-agent worker] … exited with` (complete rc only) |
| Explicit provider PID alive | Verified | `coordinator/steps/abnormal.rs::agent_process_liveness` |
| Pane foreground command / provider attribution | Verified (weaker) | same; `leader/provider_attribution.rs`; `os_probe.rs` (900 ms / 10 ms) |
| Pane exists only | **Unknown (Unverifiable)** | `tmux_backend.rs::{liveness, has_pane}` |

- T1-1 `SpawnResult.child_pid` is the pane PID; under the worker wrapper it is a shell. It MUST NOT be reported as the provider PID.
- T1-2 Pane `Live` is a precondition, not Tier 1 evidence. A pane after provider exit runs an inert tail.
- T1-3 For destructive decisions the safe default differs by consumer: stop treats non-`Dead` as present (avoid orphaning); cohort proofs treat `Unknown` as unproven (avoid false claims). Both are explicit policies, not one boolean.

### 8.3 Tier 2 — Pane readiness

Question: *can this pane accept a delivery right now without it being swallowed by a modal?*

Prerequisites: endpoint bound to the selected team; pane addressable by exact id on that endpoint;
session owner marker matches workspace/team/generation.

| Provider | Readiness source | Baseline |
|---|---|---|
| Codex | `StartupHook`: numbered trust shape, update menu, ready banner (recency-ordered) | 30 checks × 0.5 s at spawn; delivery peek Tail 80 |
| Claude/ClaudeCode | `StartupHook`: active Yes-trust shape, ready shape | same |
| Pi, CursorAgent, Grok, GeminiCli | none declared | `NoStartupModals` — **declared, not observed ready** |

- T2-1 `handled` in a startup outcome means an action was attempted; send failures are currently recorded as handled. It is not proof the modal was dismissed (Tier 2 `Verified` requires a re-observation).
- T2-2 Restart "readiness" (`session_created ∧ pane_addressable ∧ coordinator_alive`, 30 s / 200 ms) is **infrastructure readiness**. It MUST be labelled as such and MUST NOT be presented as Tier 2 or Tier 3.
- T2-3 A new provider with any first-run modal (trust, update, login, telemetry consent) MUST declare a `StartupHook` with literal shapes and key actions, or document why delivery is safe without one.

### 8.4 Tier 3 — Protocol / MCP readiness

Question: *can this agent make Team MCP calls that return?*

| Sub-level | Evidence | Baseline source |
|---|---|---|
| T3a Server handshake | The Team MCP server for this agent id **and spawn epoch** started and answered `tools/list` | `mcp_server/wire.rs` writes `mcp.server_started` and `mcp.tools_list_response_written` with `agent_id`, `owner_team_id`, `pid`, `ppid`; `spawn_epoch` is `"unavailable"` and no production consumer reads it (D-13) |
| T3b Client binding | The provider runtime reports the Team tools bound | Pi extension: `RegistrationSelected` → `ToolsAvailable` (25 ms poll, 30 s budget); proxy-only stays `RegistrationSelected` |
| T3c Owned round-trip | An owned, non-error tool result for this agent epoch | Pi extension `ToolsVerified` (all three tools returned); *(target)* generic: first owned MCP call recorded by the Team MCP server for this epoch |

- T3-1 Business success is **not** a tier. `ToolsVerified` explicitly states "business success requires the original results".
- T3-2 Cold quick-start reports `PendingToolLoad` for running seats and `Degraded` for non-running seats (`lifecycle/launch/readiness.rs::quick_start_worker_readiness`). The outer `Ready` name does not mean tools are ready.
- T3-3 A new provider MUST declare which T3 sub-levels it can observe. T3a is provider-agnostic *(target)*: once the server marker carries the spawn epoch and has a consumer, every provider gains T3a without provider code.
- T3-4 Providers whose MCP connection is lazy (connects on first tool use) cannot reach T3a before the first call; they MUST NOT be reported as Tier 3 `Negative` for that reason.

### 8.5 Budgets (baseline)

| Wait | Budget | Proves |
|---|---|---|
| Spawn identity | 500 ms / 25 ms | returned pane belongs to the requested session/window |
| tmux command | 5 s nominal, 1→25 ms poll | that command's subprocess budget only |
| OS probe | 900 ms / 10 ms | foreground/root pgrp sample |
| Startup handler | 30 checks × 0.5 s | modal handling attempted |
| Restart convergence | 12 s / 250 ms (overridable) | backing/capture convergence |
| Restart readiness | 30 s / 200 ms (overridable, zero allowed) | infrastructure facets only |
| Pi tools | 30 s / 25 ms | T3b, inside the provider |

### 8.6 Who needs which tier

| Consumer | Needs | Must not claim |
|---|---|---|
| Delivery | Tier 1 not `Negative`; Tier 2 not `Blocked` | that the agent will reply |
| Quick-start report | Tier 1 per seat | tool readiness (reports `PendingToolLoad`) |
| Restart | infrastructure + Tier 1 | Tier 3 |
| Abnormal detector | Tier 1 + fresh explicit fault | crash without a fresh explicit error (`dead_only` is suppressed) |
| Acceptance | Tier 3c + natural return + business result | other models/auth/platforms |

---

## 9. Session and fork contract

### 9.1 Session identity and resume

| Provider | Fresh session id | Resume key | Backing | Semantic reader |
|---|---|---|---|---|
| Codex | captured after launch by (cwd, spawned_at) | session id (`codex resume SID`) | JSONL rollout | yes |
| Claude/ClaudeCode | preassigned `--session-id` | session id (`--resume`) | project transcript / event log | yes (incl. background tasks) |
| Pi | preassigned id + exact session path | **exact path** (`--session PATH`) | exact JSONL in seat root | no |
| CursorAgent | captured chatId (no invented id) | chatId | chat archive | no |
| Grok | preassigned `--session-id` | session id | cwd archive dir | no |
| GeminiCli | none | none (`caps.resume = false`) | none | no |

- S-1 A preassigned id is a *prediction*; the session is `captured` only after backing evidence matches it. Leader sessions are excluded from worker capture.
- S-2 Resume MUST be backed by `probe_backing` evidence. If backing is missing, the lifecycle MUST refuse or require explicit authorization for a fresh start; it MUST NOT silently start fresh.
- S-3 Resume admission is per surface. The baseline primitive gate (`session_is_resumable`: id present ∧ not `compatible_api` ∧ `caps.resume`) differs from typed plans (Claude/Grok check id only; Cursor checks chatId bytes; Pi requires exact path). A new provider declares its gate explicitly *(target)*; it MUST NOT copy one provider's gate implicitly.

### 9.2 Fork kinds

| Kind | Agent identity | Session result | Physical action | Success evidence | Reversible? |
|---|---|---|---|---|---|
| `NewSeatSnapshot` | new agent id | new session id, full rekeyed backing | stage snapshot → register → spawn through the common worker wrapper | snapshot verified + spawn identity + commit receipt | yes, by guarded compensation |
| `InPlaceBranch { command, screen_mark }` | **same** agent id | `None` (provider-internal) | bare slash command, bracketed=false, initial 1 + ≤ 7 Enters | literal screen mark | **no** |
| `NativeArgv` | new seat | provider-native new session | `codex fork SID` / `--resume … --fork-session` | captured new session id | kill own new pane only |
| `Unsupported { reason }` | — | — | **zero keys, zero seats** | refusal text | n/a |

Baseline mapping (public `fork-agent`):

| Provider | Subscription | Other auth | Note |
|---|---|---|---|
| Claude/ClaudeCode | `InPlaceBranch { "/branch", "Branched conversation" }` | Unsupported | `/fork <directive>` is a background-agent command, not a session branch; a distinct `--as` id is refused |
| Grok | `InPlaceBranch { "/fork", "forked from" }` | Unsupported | |
| Pi | `NewSeatSnapshot` | Unsupported (Pi roles are subscription-only) | `caps.fork = false` yet public fork works |
| Codex | `Unsupported { Unverified }` | Unsupported | `caps.fork = true` and `fork_plan` exist; public path refuses |
| CursorAgent | `Unsupported { NotSupported }` | Unsupported | chatId resume does not imply fork |
| GeminiCli | `Unsupported { Unverified }` | Unsupported | |

Rules:

- F-1 `ForkPolicy` is the only source for public `fork-agent` dispatch *(target)*. `ProviderCaps.fork` is not consulted for public behavior.
- F-2 Each kind reports its **origin** in the result. An `InPlaceBranch` "verified" (screen mark) is never presented as a new seat or as a rollback-capable transaction.
- F-3 A new provider starts at `Unsupported { Unverified }` and moves only with live evidence of the command, the mark (or backing), and the failure texts.
- F-4 Context copy / clone operations (`provider/session/context_fork.rs`) are not forks and MUST NOT be reported as such.

### 9.3 `NewSeatSnapshot` transaction (normative phases)

| Phase | Requirement |
|---|---|
| p0 preflight | source/target/team are valid path components; target id distinct; no collision in state/spec/routing/runtime/managed role/pane; source has a fully captured tuple (session id, exact path, cwd, spawn cohort) — a pending id is insufficient |
| p1 stable snapshot | open with no-follow, regular file; two full reads with identical bytes + metadata; validate header/version/id/canonical absolute cwd and the full body id/parent chain; **new id, header rekeyed with source ancestry, body bytes preserved** — never a screen copy or a tail |
| p2 register | re-check source; compare-own-bytes spec update only for target/routing; seed captured fork snapshot; scoped repository registration preserving concurrent fields |
| p3 spawn | re-read target; materialize exact-path resume; spawn through the common worker wrapper; verify own pane/session/window |
| p4 commit | mark resumed with the seeded tuple; commit the target row; clear target health; ensure coordinator |
| p5 receipt | keep the role only after receipt/audit succeeds; the receipt is a backing/transaction receipt, **not** Tier 3 |
| compensation | prove no new writer; kill only the pane this transaction created; roll back backing/row/spec only if they still equal what this transaction wrote; if new records were appended or ownership is ambiguous, **preserve** and report preserved — never delete new user context |

### 9.4 `InPlaceBranch` command contract

- B-1 Inject the bare command with `TextSkipConsumptionPoll`, `bracketed = false`, floor 0.
- B-2 Press Enter once, then poll the screen (Tail 40, 80 ms apart) for the mark; re-press Enter only while neither the mark nor a known failure text is visible; total ≤ 8 (initial 1 + `MAX_ENTER_RETRIES - 1`). Never re-paste.
- B-3 Recognize literal failure texts (Claude: `Failed to branch conversation: No conversation to branch`).
- B-4 The result keeps the source agent id and `session_id = None`.

---

## 10. Teardown contract

### 10.1 Common rules

- TD-1 Every destructive effect acts on a receipt: exact pane id on the bound endpoint, exact owned session, or a PID whose OS argv contains the exact pair `--workspace <canonical workspace>`. Never on provider name, process name or bare cwd.
- TD-2 Effects are ordered and reported in order. In scoped shutdown, per-PID effects run **before** the session ownership gate; a later "destructive cleanup refused" for an unknown session does not mean zero effects happened.
- TD-3 Signals: TERM → 150 ms → fresh re-audit of ppid/command/owner → KILL; a failed audit skips the signal; a failed probe refuses escalation. No `killall`/`pkill`.
- TD-4 The tmux server and socket are never killed or deleted, even for an owned empty endpoint (`server_and_socket_cleanup_disabled_fail_closed`).
- TD-5 Results keep `ok`, `status`, `phase`, `verification_degraded` and residuals separately. Administrative "stopped" is not physical cleanliness.

### 10.2 Provider teardown declaration

| Class | Meaning | Examples (baseline) |
|---|---|---|
| Owned, removable on `remove` | created by this seat's materialization receipt | Pi seat wrapper (exact path) |
| Owned, preserved | created by the seat but holds user context | Pi sessions; Grok/Cursor archives; Codex/Claude rollouts/transcripts |
| Shared, never removed by a seat | used by several seats or by the user | Cursor shared rules; workspace `.grok/config.toml` keys of other seats |
| Forbidden | outside the workspace | user HOME config (`~/.gemini/settings.json`, `~/.claude`, `~/.codex`) |

### 10.3 Global writes

- G-1 A `GlobalInstaller` MCP carrier MUST produce a receipt containing a byte backup and a restore procedure before writing, and MUST be reachable from a documented public path. The baseline `install_mcp` writes `config.raw` directly with no backup/merge and is not called by normal spawn (D-09); it must not be wired as-is.
- G-2 Seat teardown never acquires authority over HOME files because a provider has `writes_global_settings = true`.

---

## 11. Maturity levels

Maturity is public, per provider, and is the honest answer to "is provider X supported?".

| Level | Name | Required evidence |
|---|---|---|
| L0 | Registered | enum + wire + binary + descriptor totality (D-1) |
| L1 | Launchable | fresh argv golden; spawn reaches Tier 1 `Verified` |
| L2 | Instructable | prompt carrier `Yes`; worker delivery reaches E4 with the declared input profile |
| L3 | Round-trip | MCP carrier `Yes`; live natural return (E7) under captured identity |
| L4 | Durable | resume from captured backing with a second natural return; scoped shutdown clean |
| L5 | Forkable | at least one non-`Unsupported` fork kind verified live with its evidence |

Baseline status (recorded live evidence at the anchored baseline, not a promise for every model):

| Provider | Highest level with recorded live evidence | Notes |
|---|---|---|
| Codex | L4 | quick-start → natural return → shutdown → resume → natural return → shutdown |
| Pi | L4 | same case; public new-seat fork not re-verified at this baseline |
| Claude/ClaudeCode, CursorAgent, Grok | not re-verified at this baseline | historical evidence only; treat as Declared/Reachable until re-run |
| GeminiCli | L1 (Declared/Reachable only) | `gemini [--model RAW]` builds; no prompt/MCP/bypass/effort carrier; no public leader verb; no catalog; no resume/fork |

---

## 12. Tutorial: onboarding a seventh provider

The worked example is a hypothetical CLI **Nova** (`nova`). Every Nova fact below is invented for the
tutorial; a real provider replaces them with observed facts.

### Step 0 — Native discovery record (before any code)

Capture, with the CLI version, in a discovery record kept with the change:

1. Binary name, version command, install check.
2. Model listing command and a raw sample (bytes), including notices, defaults, aliases, errors when logged out.
3. Effort/reasoning flag and its accepted literals; behavior on an unknown literal.
4. System prompt carrier (flag, file, config) and size limits.
5. MCP configuration mechanism (flag, file, directory overlay, runtime) and its scope (argv, cwd, HOME).
6. Permission/approval flags and their default when absent.
7. Session storage: path layout, id format, whether a fresh id can be preassigned, resume and fork flags.
8. TUI facts (one capture each): composer after a short paste and after a ≥ 16 KiB paste, fold placeholder text, busy indicators, queue/confirmation footers, startup modals (trust, update, login), what a second Enter does during an active turn, whether bracketed paste is honored.
9. Exit behavior: exit codes, what remains on screen.

Nova (invented): `nova --version`; `nova models --json` → `[{"id","label","default"}]`;
`--model ID`; `--reasoning low|medium|high`; `--system-file PATH`; `--mcp-config FILE`;
`--yes` bypass; sessions `~/.nova/sessions/<uuid>.jsonl`, fresh `--session-id UUID`, resume
`--resume UUID`, no fork. TUI: paste folds to `‹pasted 1.2 KB›`; multi-line pastes show
`⏎ again to send` and need a second Enter; busy shows `⠋ thinking`; first run in a folder shows
`Trust this folder? › Yes` (Enter accepts).

### Step 1 — Identity (L0)

- Add `Provider::Nova` to `model/enums.rs::Provider`; wire `"nova"`, aliases `["nova"]`, binary `"nova"` in `provider/wire.rs`; add to `ALL_PROVIDERS`.
- Build. The compiler lists every **exhaustive** `match`. Then search for **wildcard** arms that silently give Nova generic behavior; in the baseline these include `BasicProviderAdapter::build_command_plan` (`_ =>` argv-only fallback), `handle_startup_prompts_outcome` (`_ =>` no handler), `enable_fast_mode` (`_ => false`), `paste_to_submit_floor_for_recipient` (`_ => ZERO`), and `resume_backing_probe_for_agent` (`!provider_supports_resume`). Each wildcard must become an explicit decision.
- For scale: in the baseline, source files outside test directories reference `Provider::Grok` in 28 files (51 sites) and `Provider::GeminiCli` in 21 files (38 sites); inline test modules are included in those counts. Expect that blast radius until the descriptor (§2.1) exists.
- Decide `leader_verb`: `None` unless a public native leader is in scope.

### Step 2 — Descriptor facts (S2–S4)

```rust
// Illustrative — invented Nova facts.
ProviderDescriptor {
    identity:  Identity { wire: "nova", aliases: &["nova"], binary: "nova", leader_verb: None },
    maturity:  Maturity::L0,
    model:     ModelPolicy::Passthrough { trim: Trim::None },
    effort:    EffortPolicy {
        admission: [Pass, Pass, Pass, Reject("effort 'xhigh' is not supported by nova"),
                    Reject(MAX_ERR), Reject(ULTRA_ERR)],
        carrier: EffortCarrier::Flag("--reasoning"),
        inherit_team_default: true,
    },
    auth:      AuthPolicy { modes: &[AuthMode::Subscription], credential_mapper: None },
    bypass:    BypassPolicy { when_true: &["--yes"], when_false: &[] },
    prompt:    PromptCarrier::FileFlag("--system-file"),   // needs Materialize (S6)
    mcp:       McpCarrier::InlineOrFileFlag("--mcp-config"),
    tools:     ToolNaming::DoubleUnderscore,                // verify in Step 0 capture
    session:   SessionPolicy { fresh: PreassignedFlag("--session-id"), resume: SessionIdFlag("--resume"),
                               backing: BackingKind::Jsonl, semantic_reader: false },
    fork:      ForkPolicy::all(ForkKind::Unsupported { reason: Unverified }),
    input:     InputProfile { /* Step 6 */ },
    startup:   StartupPolicy::Hook,                          // trust modal
    probes:    ProbeSources { process: Generic, pane: StartupHook, protocol: &[T3a] },
    workspace: WorkspacePolicy::PerSeatArgv,                 // MCP is argv-scoped: no cwd exclusivity
    teardown:  TeardownPolicy { owned_removable: &["system prompt file", "mcp config file"],
                                preserved: &["~/.nova/sessions/<id>.jsonl"], forbidden_global: &["~/.nova"] },
}
```

Note the deliberate new effort row: Nova rejects `xhigh`. The admission table is data; it must not be
derived from another provider's row, and `is_supported_by`/`resolve_for_provider` plus every error
wrapper get explicit tests.

### Step 3 — Plan hook (S5) — pure

- Write `nova_base_command(..) -> Vec<String>` in `provider/adapters/nova.rs` using `provider/command_helpers.rs` (`append_pair`, `append_opt_pair`); order: binary, bypass, model, effort, system-file, mcp-config, session.
- Add an explicit `Provider::Nova` arm in `build_command_plan`, `build_resume_command_plan`, `fork_plan` (refuse). Return `expected_session_id` because Nova accepts `--session-id`.
- Golden tests: exact argv for (bypass on/off) × (model none/raw/with spaces) × (each effort literal) × (resume).

### Step 4 — Materialize (S6) and teardown declaration (S12)

- Write the system-prompt file and MCP config under the seat's runtime directory, mode 0600, and return their paths in a receipt. Call this from **both** cold (`spawn_agents`) and dynamic (`spawn_agent_window`) paths through one function (P-1).
- Remove only receipt paths on `remove`; never touch `~/.nova`.

### Step 5 — Session hook (S11)

- `provider/session_scan/nova.rs`: find `~/.nova/sessions/<expected>.jsonl` created after `spawned_at` for this cwd; return `CapturedSession` with confidence; exclude leader sessions.
- `resume_backing_probe_for_agent`: add an explicit Nova arm that checks the exact file.
- `provider/wire.rs::requires_resume_backing`: include Nova only after the probe exists.

### Step 6 — Input profile (S10)

Start from the generic profile; change a slot only with Step 0 evidence.

```rust
InputProfile {
    bracketed_paste: true,
    paste_identity: PasteIdentity::Placeholder("‹pasted "),   // fold text from Step 0
    paste_ready: PasteReady::TokenOrFold,
    paste_to_submit_floor: Duration::ZERO,                   // raise only if a first Enter is lost
    submit: SubmitSequence { keys: &[Key::Enter], inter_key_delay: Duration::ZERO },
    resubmit: ResubmitPolicy::Generic { max_total_submits: 3, wrap_gap_extra: 1 },
    mark_actions: &[MarkAction { mark: "⏎ again to send", key: Key::Enter,
                                 max_presses: 1, interval: Duration::from_millis(50), tail: 40 }],
    trailer: Trailer::None,
}
```

Why a `MarkAction` and not `SubmitSequence { keys: &[Enter, Enter] }`: the confirmation appears only
for multi-line pastes, so an unconditional second Enter would submit an empty turn (or interrupt) for
single-line messages (N-1). In the baseline there is no descriptor; the equivalent is a
recipient-scoped extension of `provider/submit_now.rs` called from `deliver_prepared_message`, keyed
on the recipient provider (do not add Nova's mark to the global Grok scan).

### Step 7 — Startup hook and probes (S9)

- Startup: shape `Trust this folder?` with `› Yes` active → `[Enter]`; stop on the ready composer; record the CLI version. Dispatch it from `handle_startup_prompts_outcome` and add Nova to `recipient_pane_has_actionable_startup_prompt` so messages wait in `queued_until_trust`.
- Probes: Tier 1 generic; Tier 2 via the startup hook; Tier 3: none client-side, so Nova stays `PendingToolLoad` until T3a/T3c evidence exists.

### Step 8 — Catalog (optional, S1)

Follow `docs/reference/provider-model-discovery.md`: one bounded native observation, full-source
validation, native order, explicit unsupported for providers without a catalog, fixture tests for
empty / malformed / duplicate / logged-out outputs.

### Step 9 — Fork

Keep `Unsupported { Unverified }`. Public `fork-agent` must refuse with zero keys and zero seats.

### Step 10 — Verification ladder

| Gate | Evidence |
|---|---|
| L0 | enum/wire/binary tests; all wildcard arms reviewed |
| L1 | argv goldens; live spawn with Tier 1 `Verified` |
| L2 | live delivery of a short and a ≥ 16 KiB message reaches E4; the `⏎ again to send` mark is pressed exactly when present |
| L3 | live natural return via `send_message` and `report_result` |
| L4 | shutdown → resume from captured backing → second natural return → scoped shutdown with empty owned residuals |
| L5 | not claimed |

Run live acceptance from a clean test context with only public steps; record the CLI version, the
candidate binary hash and the raw receipts. Publish the reached maturity level.

### 12.1 Onboarding checklist

- [ ] Step 0 discovery record with CLI version and raw captures
- [ ] Enum, wire, aliases, binary, `ALL_PROVIDERS`; every exhaustive match and every wildcard arm decided explicitly
- [ ] Model policy; effort admission row with byte-stable inner errors; inheritance flag
- [ ] Bypass true/false argv; auth modes; credential mapper or `None`
- [ ] Prompt carrier and MCP carrier both `Yes` or explicitly `NotCarried` with a surfaced warning
- [ ] Pure plan builder + goldens (fresh, resume, refused fork)
- [ ] Materialization through one function for cold and dynamic paths; receipt; 0600 files; teardown classes
- [ ] Session capture, backing probe, resume gate
- [ ] Input profile: floor, resubmit, mark actions, paste identity — each non-default value cites evidence
- [ ] Startup hook (or documented absence) and `queued_until_trust` gating
- [ ] Probe sources per tier declared; no tier claimed without evidence
- [ ] Fork policy (default `Unsupported { Unverified }`)
- [ ] Optional catalog per the model discovery contract
- [ ] Live ladder L1 → L4 receipts; maturity level published
- [ ] No HOME writes; no process-name matching; no Escape/C-c/CSI 201

---

## 13. Deviation register (baseline vs contract)

These are recorded differences between the anchored baseline and this contract. **None is an
authorization to change behavior.** Each migration needs its own scope, characterization tests that
lock current bytes first, and its own acceptance.

| ID | Baseline fact | Contract position | Migration note |
|---|---|---|---|
| D-01 | `ProviderCaps` has four booleans (`resume`, `fork`, `native_mcp_config`, `writes_global_settings`) | typed facets + evidence (§2.1) | `native_mcp_config = false` does not mean "no MCP"; do not reinterpret existing bools |
| D-02 | Keystroke variance via thread-locals (`with_paste_to_submit_floor`, `with_cursor_single_enter`) set only in worker delivery | explicit `SubmitPolicy` (K-1) | leader fallback pane silently gets generic defaults |
| D-03 | `payload_token_marker` takes the **first** token occurrence in the body | out-of-band typed token (T-3) | a quoted token in content can misidentify consumption |
| D-04 | `InjectReport.attempts = consumption_attempts + capture_retries` | separate counters (§7.6) | never read `attempts` as Enter count |
| D-05 | `recipient_requires_single_enter` / `recipient_allows_explicit_queue_flush` (Cursor + Pi) defined and tested, no production caller | Pi profile must be decided with evidence | wiring them changes Pi physical behavior; needs live acceptance |
| D-06 | Grok `Enter:send now` scan runs for every non-Cursor recipient | mark actions scoped by recipient profile (N-5) | today it is zero keys unless the mark appears |
| D-07 | `validate_model` returns `Ok(true)` for every non-Pi provider and has no normal-path caller | typed `Unsupported` (H-1) | do not use it as a gate |
| D-08 | `build_command_plan` `_ =>` fallback drops prompt/MCP/bypass/effort (GeminiCli) | `CarrierReport` (§4.4) | Gemini silently runs without Team prompt/MCP |
| D-09 | `install_mcp` writes `~/.gemini/settings.json` without backup/merge, comment claims backup; not called by normal spawn | `GlobalInstaller` receipt (G-1) | must not be wired as-is |
| D-10 | `status_patterns` regexes defined; runtime activity classifier does not consume them | W-3 | do not cite them as runtime behavior |
| D-11 | Copilot/Cursor/Grok materialization arms duplicated in `lifecycle/launch/spawn.rs` and `lifecycle/restart/common.rs`; Codex `/fast` toggle only on cold spawn | one S6 entry (P-1) | cold/dynamic parity must be characterized first |
| D-12 | `caps.fork` disagrees with public fork (Codex true/unverified; Pi false/supported) | `ForkPolicy` only (F-1) | keep refusal texts byte-stable |
| D-13 | `mcp.tools_list_response_written` written without spawn epoch and without consumer | T3a source (T3-3) | epoch binding is a prerequisite |
| D-14 | `POST_SUBMIT_CONSUMPTION_ATTEMPTS = 3` / `POST_SUBMIT_CONSUMPTION_POLL_MS = 60` declared but the live loop uses inline `12 × 100 ms` | single source for P7 values | doc and constants currently disagree |
| D-15 | `provider.effort_unsupported` payload uses `geminicli` (Debug lowercase), not `gemini_cli` | byte-stable until a migration | do not "fix" while unifying wire names |
| D-16 | `escape_argv` constructed then discarded; `Key::Escape`, CSI-201 helper unused on this path; `prepare_pane_for_submit` ignores `close_bracketed_paste` | X-1 | dead paths, not behavior |
| D-17 | primitive vs typed-plan resume/fork auth gates differ per provider | explicit per-provider gate (S-3) | do not unify by copying one gate |
| D-18 | `verify_started_pi_model` defined + unit tests, no production caller | started-model verification is Declared only | |

---

## 14. Conformance test catalogue

| Facet | Test kind | Must lock |
|---|---|---|
| Identity | unit | wire/alias round-trip; canonical-only surfaces reject aliases |
| Effort | table test 9 providers × 6 literals | S/I/E, inner error bytes, inheritance, event payload bytes |
| Model | unit | trim/shape/exact rules per policy; no TEAM default fill |
| Plan | golden | full argv order for each flag combination; `expected_session`; carriers |
| Materialize | tempdir integration | created vs pre-existing paths; 0600; idempotence; cold = dynamic |
| Input | fake-transport integration | key counts per channel; no Escape/C-c/CSI201; no re-paste; mark actions only on mark |
| Envelope | unit | renderer bytes; no trailing LF; ≥ 16 KiB path; token selection |
| Probes | unit + fake transport | tri-state preservation; no upward inference; epoch binding |
| Session | fixture | capture attribution; backing probe; refusal when missing |
| Fork | integration | refusal = zero keys/zero seats; in-place ≤ 8 Enters; snapshot compensation preserves appended records |
| Teardown | integration | receipt-only effects; order; server/socket untouched |
| Live | acceptance | maturity ladder receipts (§11) |
