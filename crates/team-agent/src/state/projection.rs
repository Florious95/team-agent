//! projection:team 选择 / top-level 派生视图(真相源 `state.py`,0.2.6 Family B C6-C8)。
//!
//! `state.teams` 是唯一候选源(`status == "alive"`);顶层 `session_name`/`team_dir`/`agents`/`tasks`
//! 是「当前 team」的**派生视图**,不算独立候选。选 team 用时把 `teams[key]` 投影回扁平 dict
//! (`_project_top_level_view`),并保留 `team_owner`/`leader_receiver`/`coordinator` 等辅助态。
//!
//! 纯函数操作 `serde_json::Value`;`select_runtime_state`/`resolve_team_scoped_state` 经
//! `persist::load_runtime_state` 读盘。键序对齐 Python dict(preserve_order):`setdefault`→
//! 「缺则末尾插」,`d[k]=v`→「在则原位更新,缺则末尾插」(serde_json::Map = IndexMap)。
//! §10:无 unwrap/expect/panic(全程 owned `Map`,非 object 优雅退化为空 object)。

use std::path::Path;

use serde_json::{json, Map, Value};

use super::StateError;
use crate::state::persist::{
    load_runtime_state, save_runtime_state_with_deleted_agents,
    save_runtime_state_with_lifecycle_topology_authority,
    save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip,
    save_runtime_state_with_team_tombstone_lifecycle_topology_authority,
    save_runtime_state_with_team_tombstoned_agents,
};

/// `team_state_key`(`state.py:93`):从 team_dir(.name)/spec_path(.parent.name)派生 team key,
/// 跳过 `.team`/`runtime`;兜底 `session_name` 或 `"current"`。
pub fn team_state_key(state: &Value) -> String {
    if let Some(team_key) = state
        .get("team_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
    {
        return team_key.to_string();
    }
    for field in ["team_dir", "spec_path"] {
        // Python `if not value: continue` —— None/空串 falsy 跳过。
        let value = match state.get(field).and_then(Value::as_str) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };
        let path = Path::new(value);
        let key = if field == "team_dir" {
            path.file_name()
        } else {
            path.parent().and_then(Path::file_name)
        };
        let key = key
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !key.is_empty() && key != ".team" && key != "runtime" {
            return key;
        }
    }
    state
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or_else(|| "current".to_string(), str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerTeamResolution {
    Canonical(String),
    LegacyAlias {
        requested: String,
        canonical: String,
    },
    Unresolved {
        requested: String,
    },
    Ambiguous {
        requested: String,
        matches: Vec<String>,
    },
}

impl OwnerTeamResolution {
    pub fn canonical_key(&self) -> Option<&str> {
        match self {
            OwnerTeamResolution::Canonical(key)
            | OwnerTeamResolution::LegacyAlias { canonical: key, .. } => Some(key),
            OwnerTeamResolution::Unresolved { .. } | OwnerTeamResolution::Ambiguous { .. } => None,
        }
    }
}

pub fn resolve_owner_team_id(state: &Value, owner_team_id: &str) -> OwnerTeamResolution {
    let requested = owner_team_id.trim();
    if requested.is_empty() {
        return OwnerTeamResolution::Unresolved {
            requested: owner_team_id.to_string(),
        };
    }
    let teams = state.get("teams").and_then(Value::as_object);
    if teams.is_some_and(|teams| teams.contains_key(requested)) {
        return OwnerTeamResolution::Canonical(requested.to_string());
    }
    if teams.is_none_or(Map::is_empty) {
        let active = state
            .get("active_team_key")
            .and_then(Value::as_str)
            .unwrap_or("");
        let derived = team_state_key(state);
        if active == requested || derived == requested {
            return OwnerTeamResolution::Canonical(requested.to_string());
        }
        if !active.is_empty() {
            return OwnerTeamResolution::LegacyAlias {
                requested: requested.to_string(),
                canonical: active.to_string(),
            };
        }
        if derived != "current" {
            return OwnerTeamResolution::LegacyAlias {
                requested: requested.to_string(),
                canonical: derived,
            };
        }
        return OwnerTeamResolution::Canonical(requested.to_string());
    }
    let Some(teams) = teams else {
        return OwnerTeamResolution::Unresolved {
            requested: requested.to_string(),
        };
    };
    let mut matches = Vec::new();
    for (key, entry) in teams {
        if legacy_owner_team_aliases(entry).any(|alias| alias == requested) {
            matches.push(key.clone());
        }
    }
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => OwnerTeamResolution::Unresolved {
            requested: requested.to_string(),
        },
        1 => OwnerTeamResolution::LegacyAlias {
            requested: requested.to_string(),
            canonical: matches.remove(0),
        },
        _ => OwnerTeamResolution::Ambiguous {
            requested: requested.to_string(),
            matches,
        },
    }
}

fn legacy_owner_team_aliases(entry: &Value) -> impl Iterator<Item = String> + '_ {
    let scalar_paths = [
        "/team/name",
        "/team/id",
        "/name",
        "/team_name",
        "/team_id",
        "/spec_name",
        "/legacy_owner_team_id",
        "/legacy_team_id",
        "/legacy_team_name",
        "/legacy_alias",
    ];
    let list_paths = [
        "/legacy_aliases",
        "/legacy_team_aliases",
        "/legacy_owner_team_ids",
        "/aliases",
    ];
    let scalars = scalar_paths
        .into_iter()
        .filter_map(|path| entry.pointer(path).and_then(Value::as_str));
    let lists = list_paths.into_iter().flat_map(|path| {
        entry
            .pointer(path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
    });
    scalars
        .chain(lists)
        .filter(|alias| !alias.is_empty())
        .map(str::to_string)
}

/// `compact_team_state`(`state.py:105`):剔除 `teams`(team entry 不嵌套全量 teams),保序。
pub fn compact_team_state(state: &Value) -> Value {
    match state.as_object() {
        Some(obj) => Value::Object(
            obj.iter()
                .filter(|(k, _)| k.as_str() != "teams")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => state.clone(),
    }
}

pub fn state_is_external_leader(state: &Value) -> bool {
    state
        .get("is_external_leader")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn state_is_managed_leader(state: &Value) -> bool {
    !state_is_external_leader(state)
}

/// `merge_workspace_team_state`(`state.py:111`):把新启动的 team 并入既有 workspace state。
pub fn merge_workspace_team_state(existing: &Value, launched: &Value) -> Value {
    let launched_key = team_state_key(launched);
    // existing 无 session_name → 直接以 launched 为基,seed teams。
    if !truthy_str(existing.get("session_name")) {
        let mut merged = to_object(launched);
        let mut teams = take_object(merged.get("teams"));
        teams.insert(launched_key, compact_team_state(launched));
        merged.insert("teams".to_string(), Value::Object(teams));
        return Value::Object(merged);
    }
    let existing_key = team_state_key(existing);
    if existing_key == launched_key {
        // 同 key → launched 为基,teams 取 existing 历史 + launched。
        let mut merged = to_object(launched);
        let mut teams = take_object(existing.get("teams"));
        teams.insert(launched_key, compact_team_state(launched));
        merged.insert("teams".to_string(), Value::Object(teams));
        return Value::Object(merged);
    }
    // 异 key → existing 为基,两个 team 都进 teams(existing 仅在缺时 seed)。
    let mut merged = to_object(existing);
    let mut teams = take_object(merged.get("teams"));
    teams
        .entry(existing_key)
        .or_insert_with(|| compact_team_state(existing));
    teams.insert(launched_key, compact_team_state(launched));
    merged.insert("teams".to_string(), Value::Object(teams));
    Value::Object(merged)
}

/// `team_state_candidates`(`state.py:131`):唯一候选源 = `state.teams` 中
/// `status=="alive"`(大小写不敏感;缺 status/空 视为 alive,但 0.5.26 起
/// legacy shutdown 残留「无 status + 全体 agents 处于终态」不再算 alive)。
/// 保留 teams 插入序。
///
/// 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.1):
/// shutdown 只标 agent stopped 不动 team status,retained state 让 selector
/// 把死队当活队,进而卡活队写入的 SaveConflict。谓词收敛到
/// [`team_is_alive_candidate`],这里只做遍历。
pub fn team_state_candidates(state: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let teams = match state.get("teams") {
        Some(Value::Object(m)) => m,
        _ => return out,
    };
    for (key, value) in teams {
        if !team_is_alive_candidate(value) {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    out
}

/// 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.1): 唯一 alive
/// 谓词。规则:
/// - 非 object → 排除;
/// - `archived_at` 非空 → 排除;
/// - 显式 `status` → 只有 case-insensitive `alive` 才算活;
/// - `status` 缺失/空 + `agents` 有条目且全体处于终态 → legacy shutdown
///   residue,排除;
/// - `status` 缺失/空 + 无 agent 或至少一个非终态 agent → 保持兼容 alive。
pub(crate) fn team_is_alive_candidate(team: &Value) -> bool {
    if !team.is_object() {
        return false;
    }
    if team.get("archived_at").is_some_and(|v| !v.is_null()) {
        return false;
    }
    let status_raw = team
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(status) = status_raw {
        return status.eq_ignore_ascii_case("alive");
    }
    if !team
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| !agents.is_empty())
        && team_has_live_leader_binding(team)
    {
        return false;
    }
    if all_present_agents_terminal(team) && !team_has_live_leader_binding(team) {
        return false;
    }
    true
}

/// 0.5.26: legacy team 缺 `status` 时,若挂着 `leader_receiver`/`team_owner`
/// 视为仍活的绑定面(0515 endpoint-convergence 家族:worker stopped 但 receiver
/// attached,restart 预检要能看到 leader_receiver 以派 `leader_receiver_socket_mismatch`)。
fn team_has_live_leader_binding(team: &Value) -> bool {
    let is_non_null_object = |key: &str| {
        team.get(key)
            .filter(|v| !v.is_null())
            .and_then(Value::as_object)
            .is_some_and(|m| !m.is_empty())
    };
    is_non_null_object("leader_receiver") || is_non_null_object("team_owner")
}

fn all_present_agents_terminal(team: &Value) -> bool {
    let Some(agents) = team.get("agents").and_then(Value::as_object) else {
        return false;
    };
    if agents.is_empty() {
        return false;
    }
    agents.values().all(|agent| {
        agent
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(agent_status_is_terminal)
    })
}

pub(crate) fn agent_status_is_terminal(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "stopped" | "removed" | "dead" | "terminated" | "failed"
    )
}

/// `format_team_candidates`(`state.py:148`):候选摘要串(key 排序;agents 排序逗号连,空→`-`)。
pub fn format_team_candidates(team_states: &Map<String, Value>) -> String {
    if team_states.is_empty() {
        return "No team state was found.".to_string();
    }
    let mut keys: Vec<&String> = team_states.keys().collect();
    keys.sort();
    let mut parts = Vec::new();
    for key in keys {
        let st = &team_states[key];
        let mut agent_keys: Vec<&String> = st
            .get("agents")
            .and_then(Value::as_object)
            .map(|a| a.keys().collect())
            .unwrap_or_default();
        agent_keys.sort();
        let agents = if agent_keys.is_empty() {
            "-".to_string()
        } else {
            agent_keys
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        let session = st
            .get("session_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-");
        parts.push(format!("{key} session={session} agents={agents}"));
    }
    "Candidates: ".to_string() + &parts.join("; ")
}

/// `_team_entry_from_state`(`state.py:159`):`teams[team_key]` 若为 dict 则返回引用。
fn team_entry_from_state<'a>(state: &'a Value, team_key: &str) -> Option<&'a Value> {
    state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .filter(|e| e.is_object())
}

pub fn read_owner<'a>(state: &'a Value, team_key: Option<&str>) -> Option<&'a Value> {
    let owner = match team_key.filter(|team| !team.is_empty()) {
        Some(team) => team_entry_from_state(state, team).and_then(|entry| entry.get("team_owner")),
        None => state.get("team_owner"),
    }?;
    let pane_id = owner.get("pane_id").and_then(Value::as_str)?;
    if !valid_owner_pane_id(pane_id) {
        return None;
    }
    Some(owner)
}

fn valid_owner_pane_id(pane_id: &str) -> bool {
    let pane_id = pane_id.trim();
    if pane_id.is_empty() {
        return false;
    }
    pane_id.starts_with('%') || pane_id.chars().all(|ch| ch.is_ascii_digit())
}

/// `_project_top_level_view`(`state.py:167`,C8):把 `teams[team_key]` 投影成扁平顶层视图。
pub fn project_top_level_view(state: &Value, team_key: &str) -> Value {
    // entry = _team_entry_from_state(...) or {} —— 空 dict 亦走 {} 分支(同内容)。
    let entry_obj: Map<String, Value> = team_entry_from_state(state, team_key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut p = entry_obj.clone(); // projection = deepcopy(entry)
                                   // setdefault session_name/team_dir ← entry.get(...)(缺则插,值可为 null)。
                                   // **0.3.24 excision** (U1-A real-machine RED v2): the ff38ab9 root-state
                                   // `or_else` fallback was reverted along with the wave-2 U1-A drift fallback,
                                   // since this projection change had no other consumer. v0.3.25 will re-introduce
                                   // this together with the writer-shape + rediscover-writer fix — see
                                   // `.team/artifacts/u1-a-realmachine-v2-fix-or-excise.md`.
    if !p.contains_key("session_name") {
        p.insert(
            "session_name".to_string(),
            entry_obj
                .get("session_name")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    if !p.contains_key("team_dir") {
        p.insert(
            "team_dir".to_string(),
            entry_obj.get("team_dir").cloned().unwrap_or(Value::Null),
        );
    }
    // d[k]=v:active_team_key 原位更新或末尾插。
    p.insert(
        "active_team_key".to_string(),
        Value::String(team_key.to_string()),
    );
    // 保全全量 teams 供消费者看兄弟 team(`state.get("teams") or {}` 的 truthy 语义)。
    p.insert("teams".to_string(), py_or_empty_map(state.get("teams")));
    let has_team_entries = state
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| !teams.is_empty());
    // Stage 3b (identity-boundary unified plan, architect direction 2026-06-23):
    // remove the top-level owner promote. Pre-3b, when `teams.<key>` lacked
    // an owner entry the projection promoted the legacy top-level
    // `state.team_owner` into the projected view — that's the
    // "copy-back/promotion" the architect §Stage 3 calls out as the
    // dual-source bug origin (stale top-level owner could be served to
    // callers of a different team's projection). Now only the canonical
    // teams.<key> branch promotes; callers needing the legacy top-level
    // path must go through `state::ownership::read_owner_for_team` whose
    // precedence rule applies the migration-precedence semantics
    // explicitly (architect §3: teams > top-level only when
    // team_state_key matches).
    if let Some(v) = entry_obj.get("team_owner") {
        p.insert("team_owner".to_string(), v.clone());
    }
    if let Some(v) = entry_obj.get("leader_receiver") {
        p.insert("leader_receiver".to_string(), v.clone());
    }
    let _ = has_team_entries; // silence unused warning; kept for clarity.
                              // coordinator:仅顶层有 key 时 setdefault(投影里没有才插)。
    if state
        .as_object()
        .is_some_and(|o| o.contains_key("coordinator"))
        && !p.contains_key("coordinator")
    {
        p.insert("coordinator".to_string(), state["coordinator"].clone());
    }
    Value::Object(p)
}

#[derive(Debug, Clone)]
pub struct TeamScopeResolution {
    pub canonical_team_key: String,
    pub state: Value,
}

/// Resolve a requested team selector once, returning both the canonical map key and its
/// projected state. Callers must carry `canonical_team_key` forward rather than deriving team
/// identity again from the projection or the original alias.
pub fn resolve_runtime_team_scope(
    workspace: &Path,
    team: Option<&str>,
) -> Result<TeamScopeResolution, StateError> {
    let state = load_runtime_state(workspace)?;
    let alive = team_state_candidates(&state);
    // Python `if team:` —— 空串 falsy,等同无 team(对抗 P1:此前 Some("") 误入 team 分支,空
    // team_dir 匹配致歧义/未找到错误串漂移)。
    let team = team.filter(|t| !t.is_empty());
    if let Some(team) = team {
        let canonical_request = if team.eq_ignore_ascii_case("current") {
            let teams = state.get("teams").and_then(Value::as_object);
            state
                .get("active_team_key")
                .and_then(Value::as_str)
                .filter(|active| {
                    alive.contains_key(*active)
                        || teams.is_none_or(Map::is_empty)
                        || teams.is_some_and(|teams| teams.contains_key(*active))
                })
                .map(str::to_string)
                .or_else(|| {
                    teams
                        .is_none_or(Map::is_empty)
                        .then(|| team_state_key(&state))
                        .filter(|derived| derived != "current")
                })
                .ok_or_else(|| {
                    StateError::TeamSelect(format!(
                        "team 'current' not found. {}",
                        format_team_candidates(&alive)
                    ))
                })?
        } else {
            team.to_string()
        };
        let team = canonical_request.as_str();
        // An exact canonical key is authoritative even when the team is terminal or excluded from
        // the default alive-candidate set. Explicit lifecycle operations must still be able to
        // address a shutdown team without falling back to the raw root projection.
        if state
            .get("teams")
            .and_then(Value::as_object)
            .is_some_and(|teams| teams.contains_key(team))
        {
            return Ok(TeamScopeResolution {
                canonical_team_key: team.to_string(),
                state: project_top_level_view(&state, team),
            });
        }
        // 无 alive 但 team 命中 active_team_key / 派生 key → 直接以全量 state 投影。
        if alive.is_empty() {
            let active = state
                .get("active_team_key")
                .and_then(Value::as_str)
                .unwrap_or("");
            if team == active || team == team_state_key(&state) {
                let mut projection = state.clone();
                match projection.as_object_mut() {
                    Some(o) => {
                        o.insert(
                            "active_team_key".to_string(),
                            Value::String(team.to_string()),
                        );
                    }
                    None => projection = json!({ "active_team_key": team }),
                }
                return Ok(TeamScopeResolution {
                    canonical_team_key: team.to_string(),
                    state: projection,
                });
            }
        }
        let matches: Vec<&String> = alive
            .iter()
            .filter(|(key, value)| team_selector_matches(team, key, value))
            .map(|(k, _)| k)
            .collect();
        if matches.len() == 1 {
            return Ok(TeamScopeResolution {
                canonical_team_key: matches[0].clone(),
                state: project_top_level_view(&state, matches[0]),
            });
        }
        if matches.len() > 1 {
            return Err(StateError::TeamSelect(
                "team selector is ambiguous. ".to_string() + &format_team_candidates(&alive),
            ));
        }
        return Err(StateError::TeamSelect(format!(
            "team {} not found. {}",
            py_repr_str(team),
            format_team_candidates(&alive)
        )));
    }
    // 无 team 参数:active 命中 → 投影;唯一 alive → 投影;无 alive → 全量;多 alive → 歧义。
    let active = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(active) = active {
        if alive.contains_key(active) {
            return Ok(TeamScopeResolution {
                canonical_team_key: active.to_string(),
                state: project_top_level_view(&state, active),
            });
        }
    }
    if alive.len() == 1 {
        let only = alive.keys().next().cloned().unwrap_or_default();
        return Ok(TeamScopeResolution {
            canonical_team_key: only.clone(),
            state: project_top_level_view(&state, &only),
        });
    }
    if alive.is_empty() {
        // 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.1):
        // 全体被标 shutdown(bare restart 场景),`active_team_key` 若命中
        // `state.teams` 则仍投影,让 `project_top_level_view` 用 nested
        // `teams[key].agents` 覆盖历史顶层注入(E2E-REST-002 前置)。
        if let Some(active_key) = state.get("active_team_key").and_then(Value::as_str) {
            if state
                .get("teams")
                .and_then(Value::as_object)
                .is_some_and(|teams| teams.contains_key(active_key))
            {
                return Ok(TeamScopeResolution {
                    canonical_team_key: active_key.to_string(),
                    state: project_top_level_view(&state, active_key),
                });
            }
        }
        let canonical_team_key = state
            .get("active_team_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| team_state_key(&state));
        return Ok(TeamScopeResolution {
            canonical_team_key,
            state,
        });
    }
    Err(StateError::TeamSelect(
        "multiple teams found in this workspace; pass --team <team> to choose. ".to_string()
            + &format_team_candidates(&alive),
    ))
}

/// `select_runtime_state`(`state.py:193`):compatibility projection wrapper.
pub fn select_runtime_state(workspace: &Path, team: Option<&str>) -> Result<Value, StateError> {
    resolve_runtime_team_scope(workspace, team).map(|resolved| resolved.state)
}

fn team_selector_matches(team: &str, key: &str, value: &Value) -> bool {
    if team == key {
        return true;
    }
    let session = value
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if team == session {
        return true;
    }
    if let Some(stripped) = session.strip_prefix("team-") {
        if team == stripped {
            return true;
        }
    }
    let dir = value.get("team_dir").and_then(Value::as_str).unwrap_or("");
    if team == dir {
        return true;
    }
    std::path::Path::new(dir)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| team == name)
}

/// `ambiguous_team_target_result`(`state.py:226`):无显式 team 且多候选 → 拒绝 dict;否则 None。
pub fn ambiguous_team_target_result(state: &Value) -> Option<Value> {
    let alive = team_state_candidates(state);
    let active = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(active) = active {
        if alive.contains_key(active) {
            return None;
        }
    }
    if alive.len() <= 1 {
        return None;
    }
    let mut candidates: Vec<&String> = alive.keys().collect();
    candidates.sort();
    Some(json!({
        "ok": false,
        "status": "refused",
        "reason": "team_target_ambiguous",
        "candidates": candidates,
        "message": "multiple teams found in this workspace; pass --team <team> to choose. ".to_string()
            + &format_team_candidates(&alive),
    }))
}

/// `resolve_team_scoped_state`(`state.py:243`):返回 `(state, refusal)`,二者恰一为 Some。
/// `team=None` 且歧义 → refusal;否则 select,RuntimeError → `team_target_unresolved` refusal。
pub fn resolve_team_scoped_state(
    workspace: &Path,
    team: Option<&str>,
) -> Result<(Option<Value>, Option<Value>), StateError> {
    if team.is_none() {
        let state = load_runtime_state(workspace)?;
        if let Some(ambiguous) = ambiguous_team_target_result(&state) {
            return Ok((None, Some(ambiguous)));
        }
    }
    match select_runtime_state(workspace, team) {
        Ok(state) => Ok((Some(state), None)),
        Err(StateError::TeamSelect(msg)) => Ok((
            None,
            Some(json!({
                "ok": false,
                "status": "refused",
                "reason": "team_target_unresolved",
                "team": team,
                "error": msg,
            })),
        )),
        Err(e) => Err(e),
    }
}

/// `save_team_scoped_state`(`state.py:594`):把 team-scoped(投影后)state 写回 workspace,**保全**
/// 多 team workspace 里其他 team 的持久态。单 team(磁盘无 `teams` 且 primary key == target)退化为
/// 纯 `save_runtime_state`(字节等价);多 team 时把本 team 落到 `teams[target_key]=compact(...)`,顶层
/// 视图按 golden 的 `existing_primary_key` 逻辑择 incoming/existing。§10:无 unwrap/panic。
pub fn save_team_scoped_state(workspace: &Path, team_state: &Value) -> Result<(), StateError> {
    save_team_scoped_state_with_deleted_agents(workspace, team_state, &[])
}

pub(crate) fn save_team_scoped_state_reapplying_after_conflict<F>(
    workspace: &Path,
    team_state: &Value,
    reapply: F,
) -> Result<(), StateError>
where
    F: FnOnce(&mut Value),
{
    match save_team_scoped_state(workspace, team_state) {
        Ok(()) => Ok(()),
        Err(StateError::SaveConflict(_)) => {
            let target_key = team_state_key(team_state);
            let mut latest = select_runtime_state(workspace, Some(&target_key))?;
            reapply(&mut latest);
            save_team_scoped_state(workspace, &latest)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn save_team_scoped_state_with_deleted_agents(
    workspace: &Path,
    team_state: &Value,
    deleted_agent_ids: &[&str],
) -> Result<(), StateError> {
    save_team_scoped_state_with_merge_exceptions(workspace, team_state, deleted_agent_ids, &[])
}

pub(crate) fn save_team_scoped_state_with_tombstone_lifecycle_topology_authority(
    workspace: &Path,
    team_state: &Value,
    agent_ids: &[&str],
) -> Result<(), StateError> {
    save_team_scoped_state_with_merge_options(workspace, team_state, &[], agent_ids, &[], agent_ids)
}

pub(crate) fn save_team_scoped_state_with_lifecycle_topology_authority(
    workspace: &Path,
    team_state: &Value,
    agent_ids: &[&str],
) -> Result<(), StateError> {
    save_team_scoped_state_with_merge_options(workspace, team_state, &[], &[], &[], agent_ids)
}

pub(crate) fn save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
    workspace: &Path,
    team_state: &Value,
    skip_capture_backfill_agent_ids: &[&str],
    topology_agent_ids: &[&str],
) -> Result<(), StateError> {
    save_team_scoped_state_with_merge_options(
        workspace,
        team_state,
        &[],
        &[],
        skip_capture_backfill_agent_ids,
        topology_agent_ids,
    )
}

fn save_team_scoped_state_with_merge_exceptions(
    workspace: &Path,
    team_state: &Value,
    deleted_agent_ids: &[&str],
    tombstoned_agent_ids: &[&str],
) -> Result<(), StateError> {
    save_team_scoped_state_with_merge_options(
        workspace,
        team_state,
        deleted_agent_ids,
        tombstoned_agent_ids,
        &[],
        &[],
    )
}

fn save_team_scoped_state_with_merge_options(
    workspace: &Path,
    team_state: &Value,
    deleted_agent_ids: &[&str],
    tombstoned_agent_ids: &[&str],
    skip_capture_backfill_agent_ids: &[&str],
    topology_agent_ids: &[&str],
) -> Result<(), StateError> {
    let target_key = team_state_key(team_state);
    let existing = load_runtime_state(workspace)?;
    // existing_primary_key = team_state_key(existing) if existing.get("session_name") else None
    let mut existing_primary_key = if truthy_str(existing.get("session_name")) {
        Some(team_state_key(&existing))
    } else {
        None
    };
    // 异 key 但顶层 session_name 与 incoming 相同 → 视为同 primary(golden:把 existing_primary_key 拉成 target）。
    let same_session = existing
        .get("session_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .is_some_and(|s| {
            team_state
                .get("session_name")
                .and_then(Value::as_str)
                .is_some_and(|t| t == s)
        });
    if existing_primary_key
        .as_deref()
        .is_some_and(|k| k != target_key)
        && same_session
    {
        existing_primary_key = Some(target_key.clone());
    }
    let existing_teams = take_object(existing.get("teams"));
    // incoming_teams = team_state.get("teams") if isinstance(dict) else None
    let incoming_teams = team_state.get("teams").and_then(Value::as_object).cloned();
    // not existing_teams and existing_primary_key == target_key → 纯 save(剔 teams)。
    if existing_teams.is_empty() && existing_primary_key.as_deref() == Some(target_key.as_str()) {
        let merged = compact_team_state(team_state);
        if !topology_agent_ids.is_empty() {
            if !tombstoned_agent_ids.is_empty() {
                return save_runtime_state_with_team_tombstone_lifecycle_topology_authority(
                    workspace,
                    &merged,
                    &target_key,
                    topology_agent_ids,
                );
            }
            if !skip_capture_backfill_agent_ids.is_empty() {
                return save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
                    workspace,
                    &merged,
                    &target_key,
                    skip_capture_backfill_agent_ids,
                    topology_agent_ids,
                );
            }
            return save_runtime_state_with_lifecycle_topology_authority(
                workspace,
                &merged,
                topology_agent_ids,
            );
        }
        if !skip_capture_backfill_agent_ids.is_empty() {
            return save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
                workspace,
                &merged,
                &target_key,
                skip_capture_backfill_agent_ids,
                &[],
            );
        }
        if tombstoned_agent_ids.is_empty() {
            return save_runtime_state_with_deleted_agents(workspace, &merged, deleted_agent_ids);
        }
        return save_runtime_state_with_team_tombstoned_agents(
            workspace,
            &merged,
            &target_key,
            tombstoned_agent_ids,
        );
    }
    // teams = deepcopy(incoming_teams or existing_teams)
    let mut teams = match incoming_teams {
        Some(m) if !m.is_empty() => m,
        _ => existing_teams,
    };
    teams.insert(target_key.clone(), compact_team_state(team_state));
    let mut merged = if existing_primary_key.is_none()
        || existing_primary_key.as_deref() == Some(target_key.as_str())
    {
        to_object(team_state)
    } else {
        to_object(&existing)
    };
    merged.insert("teams".to_string(), Value::Object(teams));
    // if not merged.get("teams"): merged.pop("teams", None) —— teams 为空 dict(falsy)则剔除。
    if merged
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        merged.remove("teams");
    }
    if tombstoned_agent_ids.is_empty() {
        if !topology_agent_ids.is_empty() {
            if !skip_capture_backfill_agent_ids.is_empty() {
                save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
                    workspace,
                    &Value::Object(merged),
                    &target_key,
                    skip_capture_backfill_agent_ids,
                    topology_agent_ids,
                )
            } else {
                save_runtime_state_with_lifecycle_topology_authority(
                    workspace,
                    &Value::Object(merged),
                    topology_agent_ids,
                )
            }
        } else if !skip_capture_backfill_agent_ids.is_empty() {
            save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip(
                workspace,
                &Value::Object(merged),
                &target_key,
                skip_capture_backfill_agent_ids,
                &[],
            )
        } else {
            save_runtime_state_with_deleted_agents(
                workspace,
                &Value::Object(merged),
                deleted_agent_ids,
            )
        }
    } else {
        if !topology_agent_ids.is_empty() {
            return save_runtime_state_with_team_tombstone_lifecycle_topology_authority(
                workspace,
                &Value::Object(merged),
                &target_key,
                topology_agent_ids,
            );
        }
        save_runtime_state_with_team_tombstoned_agents(
            workspace,
            &Value::Object(merged),
            &target_key,
            tombstoned_agent_ids,
        )
    }
}

// ---- helpers ----

/// Python truthiness of a string-or-null value(非空串 → true;null/缺/空串 → false)。
fn truthy_str(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

/// Python `x or {}`:`x` truthy 则取 `x`,否则 `{}`。
fn py_or_empty_map(v: Option<&Value>) -> Value {
    match v {
        Some(x) if super::json_truthy(x) => x.clone(),
        _ => json!({}),
    }
}

/// `v` 为 object → 克隆其 `Map`;否则空 `Map`(deepcopy(dict) 的优雅退化)。
fn to_object(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// `v.get(...) or {}` 取 object → 克隆;否则空 `Map`。
fn take_object(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

/// Python `repr()` of a `str`(与 `model::spec::py_repr_str` 同实现,本地副本免改工作模块)。
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// 比较紧凑序列化(serde_json::to_string 无空格)—— 同时锁值 + 键插入序。
    fn same_repr(a: &Value, b: &Value) {
        assert_eq!(
            serde_json::to_string(a).unwrap(),
            serde_json::to_string(b).unwrap()
        );
    }

    fn temp_ws_with_state(state: &Value) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!("ta_rs_proj_{}_{}", std::process::id(), n));
        let rt = ws.join(".team").join("runtime");
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::write(rt.join("state.json"), serde_json::to_string(state).unwrap()).unwrap();
        ws
    }

    #[test]
    fn team_state_key_cases() {
        assert_eq!(
            team_state_key(&json!({"team_dir": "/ws/.team/myteam"})),
            "myteam"
        );
        assert_eq!(
            team_state_key(&json!({"team_dir": "/ws/.team/runtime", "session_name": "sess"})),
            "sess"
        );
        assert_eq!(
            team_state_key(&json!({"spec_path": "/ws/.team/alpha/TEAM.md"})),
            "alpha"
        );
        assert_eq!(team_state_key(&json!({"session_name": "sname"})), "sname");
        assert_eq!(team_state_key(&json!({})), "current");
        assert_eq!(team_state_key(&json!({"session_name": ""})), "current");
    }

    #[test]
    fn compact_drops_teams_keeps_order() {
        let r = compact_team_state(&json!({"a": 1, "teams": {"x": {}}, "b": 2}));
        same_repr(&r, &json!({"a": 1, "b": 2}));
    }

    #[test]
    fn candidates_alive_filter_and_order() {
        // 0.5.26 (`.team/artifacts/stale-team-saveconflict-locate.md` §7.1):
        // 空 bootstrap team(无 agents)在 status 缺失时保 alive 兼容;
        // 但「status 缺失 + 全体 agent stopped」是 legacy shutdown residue,
        // 必须被剔除,防止 shutdown --keep-logs 的死队卡活队写入。
        let s = json!({"teams": {
            "alive1": {"status": "alive", "session_name": "sa"},
            "dead1": {"status": "shutdown"},
            "nostatus": {"session_name": "sn"},
            "ALIVE_CASE": {"status": "ALIVE"},
            "legacy_shutdown_all_stopped": {
                "session_name": "team-supermarket-suite",
                "agents": {"adminweb": {"status": "stopped"}}
            },
        }});
        let cands = team_state_candidates(&s);
        let keys: Vec<&String> = cands.keys().collect();
        assert_eq!(keys, vec!["alive1", "nostatus", "ALIVE_CASE"]);
    }

    #[test]
    fn format_candidates_golden() {
        let s = json!({"teams": {
            "alive1": {"status": "alive", "session_name": "sa"},
            "dead1": {"status": "shutdown"},
            "nostatus": {"session_name": "sn"},
            "ALIVE_CASE": {"status": "ALIVE"},
        }});
        assert_eq!(
            format_team_candidates(&team_state_candidates(&s)),
            "Candidates: ALIVE_CASE session=- agents=-; alive1 session=sa agents=-; nostatus session=sn agents=-"
        );
        assert_eq!(
            format_team_candidates(&Map::new()),
            "No team state was found."
        );
        let mut m = Map::new();
        m.insert(
            "k2".to_string(),
            json!({"session_name": "s2", "agents": {"b": {}, "a": {}}}),
        );
        m.insert("k1".to_string(), json!({}));
        assert_eq!(
            format_team_candidates(&m),
            "Candidates: k1 session=- agents=-; k2 session=s2 agents=a,b"
        );
    }

    #[test]
    fn project_top_level_view_golden() {
        let s = json!({
            "teams": {"t1": {"session_name": "ses1", "team_dir": "/w/.team/t1", "agents": {"a": {}}, "team_owner": {"pane_id": "%1"}}},
            "team_owner": {"pane_id": "%top"},
            "leader_receiver": {"pane_id": "%lr"},
            "coordinator": {"pid": 99},
        });
        let expected = json!({
            "session_name": "ses1", "team_dir": "/w/.team/t1", "agents": {"a": {}}, "team_owner": {"pane_id": "%1"},
            "active_team_key": "t1",
            "teams": {"t1": {"session_name": "ses1", "team_dir": "/w/.team/t1", "agents": {"a": {}}, "team_owner": {"pane_id": "%1"}}},
            "coordinator": {"pid": 99},
        });
        same_repr(&project_top_level_view(&s, "t1"), &expected);
    }

    #[test]
    fn project_top_level_view_does_not_fall_to_toplevel_owner_when_teams_exist() {
        let s =
            json!({"teams": {"t2": {"session_name": "s2"}}, "team_owner": {"pane_id": "%top2"}});
        let expected = json!({
            "session_name": "s2", "team_dir": null, "active_team_key": "t2",
            "teams": {"t2": {"session_name": "s2"}},
        });
        same_repr(&project_top_level_view(&s, "t2"), &expected);
    }

    #[test]
    fn merge_three_branches_golden() {
        same_repr(
            &merge_workspace_team_state(
                &json!({}),
                &json!({"team_dir": "/w/.team/t1", "session_name": "s1", "agents": {}}),
            ),
            &json!({"team_dir": "/w/.team/t1", "session_name": "s1", "agents": {},
                    "teams": {"t1": {"team_dir": "/w/.team/t1", "session_name": "s1", "agents": {}}}}),
        );
        same_repr(
            &merge_workspace_team_state(
                &json!({"team_dir": "/w/.team/t1", "session_name": "old", "teams": {"t1": {"session_name": "older"}}}),
                &json!({"team_dir": "/w/.team/t1", "session_name": "new"}),
            ),
            &json!({"team_dir": "/w/.team/t1", "session_name": "new",
                    "teams": {"t1": {"team_dir": "/w/.team/t1", "session_name": "new"}}}),
        );
        same_repr(
            &merge_workspace_team_state(
                &json!({"team_dir": "/w/.team/t1", "session_name": "s1"}),
                &json!({"team_dir": "/w/.team/t2", "session_name": "s2"}),
            ),
            &json!({"team_dir": "/w/.team/t1", "session_name": "s1",
                    "teams": {"t1": {"team_dir": "/w/.team/t1", "session_name": "s1"},
                              "t2": {"team_dir": "/w/.team/t2", "session_name": "s2"}}}),
        );
    }

    #[test]
    fn select_runtime_state_errors_match_python() {
        let ws = temp_ws_with_state(&json!({"teams": {
            "t1": {"status": "alive", "session_name": "s1"},
            "t2": {"status": "alive", "session_name": "s2"},
        }}));
        let e = select_runtime_state(&ws, Some("missing")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "team 'missing' not found. Candidates: t1 session=s1 agents=-; t2 session=s2 agents=-"
        );
        let e2 = select_runtime_state(&ws, None).unwrap_err();
        assert_eq!(
            e2.to_string(),
            "multiple teams found in this workspace; pass --team <team> to choose. Candidates: t1 session=s1 agents=-; t2 session=s2 agents=-"
        );

        let ws2 = temp_ws_with_state(&json!({"teams": {
            "t1": {"status": "alive", "session_name": "dup"},
            "t2": {"status": "alive", "session_name": "dup"},
        }}));
        let e3 = select_runtime_state(&ws2, Some("dup")).unwrap_err();
        assert_eq!(
            e3.to_string(),
            "team selector is ambiguous. Candidates: t1 session=dup agents=-; t2 session=dup agents=-"
        );
    }

    #[test]
    fn resolve_runtime_team_scope_current_uses_valid_active_key() {
        let ws = temp_ws_with_state(&json!({
            "active_team_key": "alpha",
            "teams": {
                "alpha": {"status": "alive", "session_name": "team-alpha", "agents": {"worker": {}}},
                "old-team": {"team_owner": {"pane_id": "%9"}}
            }
        }));
        let resolved = resolve_runtime_team_scope(&ws, Some("current")).unwrap();
        assert_eq!(resolved.canonical_team_key, "alpha");
        assert_eq!(
            resolved.state.get("active_team_key").and_then(Value::as_str),
            Some("alpha")
        );
    }

    #[test]
    fn resolve_runtime_team_scope_current_fails_without_valid_active_key() {
        let ws = temp_ws_with_state(&json!({
            "active_team_key": "missing",
            "teams": {
                "alpha": {"status": "alive", "session_name": "team-alpha", "agents": {"worker": {}}}
            }
        }));
        let error = resolve_runtime_team_scope(&ws, Some("current")).unwrap_err();
        assert!(error.to_string().contains("team 'current' not found"));
    }

    #[test]
    fn resolve_runtime_team_scope_current_keeps_legacy_single_team_state() {
        let ws = temp_ws_with_state(&json!({
            "active_team_key": "legacy",
            "team_key": "legacy",
            "session_name": "team-legacy",
            "agents": {"worker": {"status": "running"}}
        }));
        let resolved = resolve_runtime_team_scope(&ws, Some("current")).unwrap();
        assert_eq!(resolved.canonical_team_key, "legacy");
        assert_eq!(resolved.state["active_team_key"], json!("legacy"));
    }

    #[test]
    fn resolve_runtime_team_scope_current_rejects_empty_workspace_bootstrap() {
        let ws = temp_ws_with_state(&json!({}));
        let error = resolve_runtime_team_scope(&ws, Some("current")).unwrap_err();
        assert!(error.to_string().contains("team 'current' not found"));
        let saved = load_runtime_state(&ws).unwrap();
        assert_ne!(
            saved.get("active_team_key").and_then(Value::as_str),
            Some("current")
        );
        assert!(!crate::model::paths::runtime_spec_path(&ws, "current").exists());
    }

    // 对抗 P1:空串 team 必须走「无 team」分支(Python `if team:` falsy),报「multiple teams」
    // 而非「team selector is ambiguous」(后者是非空 team 多匹配)。
    #[test]
    fn select_runtime_state_empty_team_is_no_team() {
        let ws = temp_ws_with_state(&json!({"teams": {
            "t1": {"status": "alive", "session_name": "s1"},
            "t2": {"status": "alive", "session_name": "s2"},
        }}));
        let empty = select_runtime_state(&ws, Some("")).unwrap_err();
        let none = select_runtime_state(&ws, None).unwrap_err();
        assert_eq!(empty.to_string(), none.to_string());
        assert!(empty
            .to_string()
            .starts_with("multiple teams found in this workspace"));
    }

    #[test]
    fn select_runtime_state_team_id_exact_match_beats_session_alias() {
        let ws = temp_ws_with_state(&json!({"teams": {
            "current": {"status": "alive", "session_name": "dup"},
            "dup": {"status": "alive", "session_name": "actual-session"},
        }}));
        let selected = select_runtime_state(&ws, Some("dup")).unwrap();
        assert_eq!(
            selected["active_team_key"],
            json!("dup"),
            "an exact team-id match must not be ambiguous just because another team has the same session alias"
        );
        assert_eq!(selected["session_name"], json!("actual-session"));
    }

    #[test]
    fn archived_team_without_alive_status_is_not_a_default_status_candidate() {
        let candidates = team_state_candidates(&json!({"teams": {
            "archived": {"archived_at": "2026-06-04T00:00:00Z", "session_name": "old-session"},
            "current": {"status": "alive", "session_name": "current-session"},
        }}));
        assert_eq!(
            candidates.keys().cloned().collect::<Vec<_>>(),
            vec!["current".to_string()],
            "status should default to the current/live team; archived markers must not be treated as alive"
        );
    }

    #[test]
    fn select_runtime_state_single_alive_projects() {
        let ws = temp_ws_with_state(
            &json!({"teams": {"only": {"status": "alive", "session_name": "so"}}}),
        );
        let r = select_runtime_state(&ws, None).unwrap();
        assert_eq!(r["active_team_key"], json!("only"));
        assert_eq!(r["session_name"], json!("so"));
    }

    #[test]
    fn resolve_team_scoped_state_ambiguous_refusal() {
        let ws = temp_ws_with_state(&json!({"teams": {
            "t1": {"status": "alive", "session_name": "s1"},
            "t2": {"status": "alive", "session_name": "s2"},
        }}));
        let (state, refusal) = resolve_team_scoped_state(&ws, None).unwrap();
        assert!(state.is_none());
        let refusal = refusal.unwrap();
        assert_eq!(refusal["reason"], json!("team_target_ambiguous"));
        assert_eq!(refusal["candidates"], json!(["t1", "t2"]));

        let (state2, refusal2) = resolve_team_scoped_state(&ws, Some("missing")).unwrap();
        assert!(state2.is_none());
        assert_eq!(refusal2.unwrap()["reason"], json!("team_target_unresolved"));
    }
}
