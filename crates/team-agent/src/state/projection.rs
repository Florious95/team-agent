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
use crate::state::persist::{load_runtime_state, save_runtime_state};

/// `team_state_key`(`state.py:93`):从 team_dir(.name)/spec_path(.parent.name)派生 team key,
/// 跳过 `.team`/`runtime`;兜底 `session_name` 或 `"current"`。
pub fn team_state_key(state: &Value) -> String {
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
        let key = key.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
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
    teams.entry(existing_key).or_insert_with(|| compact_team_state(existing));
    teams.insert(launched_key, compact_team_state(launched));
    merged.insert("teams".to_string(), Value::Object(teams));
    Value::Object(merged)
}

/// `team_state_candidates`(`state.py:131`):唯一候选源 = `state.teams` 中 `status=="alive"`
/// (大小写不敏感;缺 status/空 视为 alive)。保留 teams 插入序。
pub fn team_state_candidates(state: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let teams = match state.get("teams") {
        Some(Value::Object(m)) => m,
        _ => return out,
    };
    for (key, value) in teams {
        if !value.is_object() {
            continue;
        }
        if value.get("archived_at").is_some_and(|v| !v.is_null()) {
            continue;
        }
        // `str(value.get("status") or "alive").lower()` —— None/空串 → "alive"。
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("alive");
        if !status.eq_ignore_ascii_case("alive") {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    out
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
            agent_keys.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",")
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
    let entry_obj: Map<String, Value> =
        team_entry_from_state(state, team_key).and_then(Value::as_object).cloned().unwrap_or_default();
    let mut p = entry_obj.clone(); // projection = deepcopy(entry)
    // setdefault session_name/team_dir ← entry.get(...)(缺则插,值可为 null)。
    if !p.contains_key("session_name") {
        p.insert("session_name".to_string(), entry_obj.get("session_name").cloned().unwrap_or(Value::Null));
    }
    if !p.contains_key("team_dir") {
        p.insert("team_dir".to_string(), entry_obj.get("team_dir").cloned().unwrap_or(Value::Null));
    }
    // d[k]=v:active_team_key 原位更新或末尾插。
    p.insert("active_team_key".to_string(), Value::String(team_key.to_string()));
    // 保全全量 teams 供消费者看兄弟 team(`state.get("teams") or {}` 的 truthy 语义)。
    p.insert("teams".to_string(), py_or_empty_map(state.get("teams")));
    let has_team_entries = state
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| !teams.is_empty());
    // owner binding is team-scoped: never borrow another team's top-level binding.
    if let Some(v) = entry_obj.get("team_owner") {
        p.insert("team_owner".to_string(), v.clone());
    } else if !has_team_entries && state.get("team_owner").is_some_and(|v| !v.is_null()) {
        p.insert("team_owner".to_string(), state["team_owner"].clone());
    }
    if let Some(v) = entry_obj.get("leader_receiver") {
        p.insert("leader_receiver".to_string(), v.clone());
    } else if !has_team_entries && state.get("leader_receiver").is_some_and(|v| !v.is_null()) {
        p.insert("leader_receiver".to_string(), state["leader_receiver"].clone());
    }
    // coordinator:仅顶层有 key 时 setdefault(投影里没有才插)。
    if state.as_object().is_some_and(|o| o.contains_key("coordinator")) && !p.contains_key("coordinator") {
        p.insert("coordinator".to_string(), state["coordinator"].clone());
    }
    Value::Object(p)
}

/// `select_runtime_state`(`state.py:193`):读 state → 按 `team` 选候选 → 投影顶层视图。
/// 歧义/未找到 → `Err(StateError::TeamSelect(msg))`(msg == Python `str(RuntimeError)`)。
pub fn select_runtime_state(workspace: &Path, team: Option<&str>) -> Result<Value, StateError> {
    let state = load_runtime_state(workspace)?;
    let alive = team_state_candidates(&state);
    // Python `if team:` —— 空串 falsy,等同无 team(对抗 P1:此前 Some("") 误入 team 分支,空
    // team_dir 匹配致歧义/未找到错误串漂移)。
    let team = team.filter(|t| !t.is_empty());
    if let Some(team) = team {
        // 无 alive 但 team 命中 active_team_key / 派生 key → 直接以全量 state 投影。
        if alive.is_empty() {
            let active = state.get("active_team_key").and_then(Value::as_str).unwrap_or("");
            if team == active || team == team_state_key(&state) {
                let mut projection = state.clone();
                match projection.as_object_mut() {
                    Some(o) => {
                        o.insert("active_team_key".to_string(), Value::String(team.to_string()));
                    }
                    None => projection = json!({ "active_team_key": team }),
                }
                return Ok(projection);
            }
        }
        if alive.contains_key(team) {
            return Ok(project_top_level_view(&state, team));
        }
        let matches: Vec<&String> = alive
            .iter()
            .filter(|(key, value)| {
                let session = value.get("session_name").and_then(Value::as_str).unwrap_or("");
                let dir = value.get("team_dir").and_then(Value::as_str).unwrap_or("");
                team == key.as_str() || team == session || team == dir
            })
            .map(|(k, _)| k)
            .collect();
        if matches.len() == 1 {
            return Ok(project_top_level_view(&state, matches[0]));
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
    let active = state.get("active_team_key").and_then(Value::as_str).filter(|s| !s.is_empty());
    if let Some(active) = active {
        if alive.contains_key(active) {
            return Ok(project_top_level_view(&state, active));
        }
    }
    if alive.len() == 1 {
        let only = alive.keys().next().cloned().unwrap_or_default();
        return Ok(project_top_level_view(&state, &only));
    }
    if alive.is_empty() {
        return Ok(state);
    }
    Err(StateError::TeamSelect(
        "multiple teams found in this workspace; pass --team <team> to choose. ".to_string()
            + &format_team_candidates(&alive),
    ))
}

/// `ambiguous_team_target_result`(`state.py:226`):无显式 team 且多候选 → 拒绝 dict;否则 None。
pub fn ambiguous_team_target_result(state: &Value) -> Option<Value> {
    let alive = team_state_candidates(state);
    let active = state.get("active_team_key").and_then(Value::as_str).filter(|s| !s.is_empty());
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
    if existing_primary_key.as_deref().is_some_and(|k| k != target_key) && same_session {
        existing_primary_key = Some(target_key.clone());
    }
    let existing_teams = take_object(existing.get("teams"));
    // incoming_teams = team_state.get("teams") if isinstance(dict) else None
    let incoming_teams = team_state.get("teams").and_then(Value::as_object).cloned();
    // not existing_teams and existing_primary_key == target_key → 纯 save(剔 teams)。
    if existing_teams.is_empty() && existing_primary_key.as_deref() == Some(target_key.as_str()) {
        let merged = compact_team_state(team_state);
        return save_runtime_state(workspace, &merged);
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
    if merged.get("teams").and_then(Value::as_object).is_some_and(Map::is_empty) {
        merged.remove("teams");
    }
    save_runtime_state(workspace, &Value::Object(merged))
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
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
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
        assert_eq!(serde_json::to_string(a).unwrap(), serde_json::to_string(b).unwrap());
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
        assert_eq!(team_state_key(&json!({"team_dir": "/ws/.team/myteam"})), "myteam");
        assert_eq!(team_state_key(&json!({"team_dir": "/ws/.team/runtime", "session_name": "sess"})), "sess");
        assert_eq!(team_state_key(&json!({"spec_path": "/ws/.team/alpha/TEAM.md"})), "alpha");
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
        let s = json!({"teams": {
            "alive1": {"status": "alive", "session_name": "sa"},
            "dead1": {"status": "shutdown"},
            "nostatus": {"session_name": "sn"},
            "ALIVE_CASE": {"status": "ALIVE"},
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
        assert_eq!(format_team_candidates(&Map::new()), "No team state was found.");
        let mut m = Map::new();
        m.insert("k2".to_string(), json!({"session_name": "s2", "agents": {"b": {}, "a": {}}}));
        m.insert("k1".to_string(), json!({}));
        assert_eq!(format_team_candidates(&m), "Candidates: k1 session=- agents=-; k2 session=s2 agents=a,b");
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
        let s = json!({"teams": {"t2": {"session_name": "s2"}}, "team_owner": {"pane_id": "%top2"}});
        let expected = json!({
            "session_name": "s2", "team_dir": null, "active_team_key": "t2",
            "teams": {"t2": {"session_name": "s2"}},
        });
        same_repr(&project_top_level_view(&s, "t2"), &expected);
    }

    #[test]
    fn merge_three_branches_golden() {
        same_repr(
            &merge_workspace_team_state(&json!({}), &json!({"team_dir": "/w/.team/t1", "session_name": "s1", "agents": {}})),
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
        assert!(empty.to_string().starts_with("multiple teams found in this workspace"));
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
        let ws = temp_ws_with_state(&json!({"teams": {"only": {"status": "alive", "session_name": "so"}}}));
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
