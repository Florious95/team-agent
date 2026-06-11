//! identity:leader receiver identity 派生 / 迁移 / first-time binding(真相源 `state.py`)。
//!
//! Python 直接读 `os.environ`/`os.getcwd()`/`Path.resolve()`;此处把**环境变量**经
//! [`IdentityEnv`] trait 注入以保持可测,cwd / 路径软链解析仍走真实进程(与 Python `resolve()`
//! 一致,Python 端同样不可注入)。`datetime.now().isoformat()` 经 `now_iso: &str` 注入
//! (真 now() 延 step 11/coordinator)。
//!
//! uuid 派生 = [`crate::model::ids::LeaderSessionUuid::derive`](sha256[:32]);输入含 NUL →
//! `Err(ModelError)`(文件系统拒绝 NUL,实践不触发)。
//!
//! §11:`apply_first_time_leader_binding` 的 own-vs-foreign 用 [`owner_gate::realpath_like`]
//! **两边 realpath 全等**才放行(禁 basename/子串/反推);拒绝 dict 字节对齐 Python(含 `pane` 字段
//! + `repr()` 错误串)。§10:无 unwrap/expect/panic。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::json_truthy;
use crate::model::errors::ModelError;
use crate::model::ids::LeaderSessionUuid;
use crate::state::owner_gate::{realpath_like, CallerIdentity};
use crate::state::projection::team_state_key;

/// 环境变量注入(`os.environ.get`)。返回原始值(含空串),未设置 → `None`。
pub trait IdentityEnv {
    fn var(&self, key: &str) -> Option<String>;
}

/// 真实进程环境(`std::env::var`)。
pub struct SystemEnv;
impl IdentityEnv for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// `os.environ.get(key) or ""` —— 未设置或空串 → `""`。
fn env_str(env: &dyn IdentityEnv, key: &str) -> String {
    env.var(key).unwrap_or_default()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `_identity_os_user`(`state.py:275`):`USER` or `USERNAME` or `""`。
pub fn identity_os_user(env: &dyn IdentityEnv) -> String {
    let user = env_str(env, "USER");
    if !user.is_empty() {
        return user;
    }
    let username = env_str(env, "USERNAME");
    if !username.is_empty() {
        return username;
    }
    String::new()
}

/// `_identity_machine_fingerprint`(`state.py:279`):team_owner/leader_receiver 的非空
/// machine_fingerprint 优先,否则 env `TEAM_AGENT_MACHINE_FINGERPRINT` or `""`。
pub fn identity_machine_fingerprint(state: &Value, env: &dyn IdentityEnv) -> String {
    for key in ["team_owner", "leader_receiver"] {
        if let Some(fp) = state
            .get(key)
            .and_then(Value::as_object)
            .and_then(|r| r.get("machine_fingerprint"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return fp.to_string();
        }
    }
    env_str(env, "TEAM_AGENT_MACHINE_FINGERPRINT")
}

/// `_identity_workspace_abspath`(`state.py:264`):按 workspace→team_dir→spec_path→fallback
/// 顺序派生 workspace 绝对路径(各分支先 `resolve()` 再做 parent 导航)。
pub fn identity_workspace_abspath(state: &Value, env: &dyn IdentityEnv, workspace: Option<&Path>) -> String {
    if let Some(ws) = state.get("workspace").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return path_str(&realpath_like(Path::new(ws)));
    }
    if let Some(td) = state.get("team_dir").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        // resolve().parent.parent
        let r = realpath_like(Path::new(td));
        let pp = r.parent().and_then(Path::parent).map_or_else(|| r.clone(), Path::to_path_buf);
        return path_str(&pp);
    }
    if let Some(sp) = state.get("spec_path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let r = realpath_like(Path::new(sp));
        let parent = r.parent();
        let grandparent = parent.and_then(Path::parent);
        // grandparent.name == ".team" ? grandparent.parent : parent
        let result = if grandparent.and_then(Path::file_name).is_some_and(|n| n == std::ffi::OsStr::new(".team")) {
            grandparent.and_then(Path::parent).map_or_else(|| r.clone(), Path::to_path_buf)
        } else {
            parent.map_or_else(|| r.clone(), Path::to_path_buf)
        };
        return path_str(&result);
    }
    // fallback: workspace or env TEAM_AGENT_WORKSPACE or cwd,再 resolve()。
    let base: PathBuf = match workspace {
        Some(w) => w.to_path_buf(),
        None => {
            let envws = env_str(env, "TEAM_AGENT_WORKSPACE");
            if !envws.is_empty() {
                PathBuf::from(envws)
            } else {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }
        }
    };
    path_str(&realpath_like(&base))
}

/// `_leader_session_uuid_for_state`(`state.py:286`)。`team_id or team_state_key(state)`。
fn leader_session_uuid_for_state(
    state: &Value,
    env: &dyn IdentityEnv,
    workspace: Option<&Path>,
    team_id: Option<&str>,
) -> Result<String, ModelError> {
    let tid = match team_id {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => team_state_key(state),
    };
    let uuid = LeaderSessionUuid::derive(
        &identity_machine_fingerprint(state, env),
        &identity_workspace_abspath(state, env, workspace),
        &identity_os_user(env),
        &tid,
    )?;
    Ok(uuid.as_str().to_string())
}

/// `_migrate_team_identity`(`state.py:295`):为缺 `leader_session_uuid` 的 team_owner /
/// leader_receiver 填入派生 uuid。返回是否有改动。
pub fn migrate_team_identity(
    state: &mut Value,
    env: &dyn IdentityEnv,
    workspace: Option<&Path>,
    team_id: Option<&str>,
) -> Result<bool, ModelError> {
    let leader_uuid = leader_session_uuid_for_state(state, env, workspace, team_id)?;
    let mut changed = false;
    for key in ["team_owner", "leader_receiver"] {
        // record 是 dict 且其 leader_session_uuid 为空/缺 → 填。
        let needs = match state.get(key).and_then(Value::as_object) {
            Some(r) => r.get("leader_session_uuid").and_then(Value::as_str).is_none_or(str::is_empty),
            None => false,
        };
        if needs {
            if let Some(obj) = state.get_mut(key).and_then(Value::as_object_mut) {
                obj.insert("leader_session_uuid".to_string(), Value::String(leader_uuid.clone()));
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// `_migrate_state_identity`(`state.py:306`):顶层(有 session_name 时)+ 每个 team 子状态迁移。
pub fn migrate_state_identity(state: &mut Value, env: &dyn IdentityEnv, workspace: &Path) -> Result<bool, ModelError> {
    let mut changed = false;
    if state.get("session_name").is_some_and(json_truthy) {
        changed = migrate_team_identity(state, env, Some(workspace), None)?;
    }
    let team_keys: Vec<String> = state
        .get("teams")
        .and_then(Value::as_object)
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    for tid in team_keys {
        // 取出 team_state &mut,就地迁移(team_id = key)。
        if let Some(team_state) = state.get_mut("teams").and_then(Value::as_object_mut).and_then(|t| t.get_mut(&tid)) {
            if team_state.is_object() {
                let c = migrate_team_identity(team_state, env, Some(workspace), Some(&tid))?;
                changed = c || changed;
            }
        }
    }
    Ok(changed)
}

/// `_caller_identity_from_env`(`state.py:316`):env override/env/derived → 5 字段 caller 身份。
pub fn caller_identity_from_env(
    state: Option<&Value>,
    env: &dyn IdentityEnv,
    team_id: Option<&str>,
    workspace: Option<&Path>,
) -> Result<CallerIdentity, ModelError> {
    let empty = json!({});
    let state = state.unwrap_or(&empty);
    let machine_fingerprint = env_str(env, "TEAM_AGENT_MACHINE_FINGERPRINT");
    let override_uuid = env_str(env, "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE");
    let env_uuid = env_str(env, "TEAM_AGENT_LEADER_SESSION_UUID");
    let leader_uuid = if !override_uuid.is_empty() {
        override_uuid.clone()
    } else if !env_uuid.is_empty() {
        env_uuid.clone()
    } else {
        // team_id or env TEAM_AGENT_TEAM_ID or team_state_key(state)
        let tid = match team_id {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                let envtid = env_str(env, "TEAM_AGENT_TEAM_ID");
                if !envtid.is_empty() {
                    envtid
                } else {
                    team_state_key(state)
                }
            }
        };
        LeaderSessionUuid::derive(
            &machine_fingerprint,
            &identity_workspace_abspath(state, env, workspace),
            &identity_os_user(env),
            &tid,
        )?
        .as_str()
        .to_string()
    };
    let source = if !override_uuid.is_empty() {
        "explicit-override"
    } else if !env_uuid.is_empty() {
        "env"
    } else {
        "derived"
    };
    let pane_id = {
        let p = env_str(env, "TEAM_AGENT_LEADER_PANE_ID");
        if !p.is_empty() {
            p
        } else {
            env_str(env, "TMUX_PANE")
        }
    };
    Ok(CallerIdentity {
        pane_id,
        provider: env_str(env, "TEAM_AGENT_LEADER_PROVIDER"),
        machine_fingerprint,
        leader_session_uuid: leader_uuid,
        leader_session_uuid_source: source.to_string(),
    })
}

/// `populate_team_owner_from_env`(`state.py:414`):已有 team_owner → 仅补迁移 uuid 后返回;
/// 否则从 env caller 身份 seed team_owner(无 pane_id → `None`)。`now_iso` = claimed_at。
pub fn populate_team_owner_from_env(
    state: &mut Value,
    source: &str,
    env: &dyn IdentityEnv,
    now_iso: &str,
) -> Result<Option<Value>, ModelError> {
    if state.get("team_owner").is_some_and(json_truthy) {
        let ws_abspath = identity_workspace_abspath(state, env, None);
        let key = team_state_key(state);
        migrate_team_identity(state, env, Some(Path::new(&ws_abspath)), Some(&key))?;
        return Ok(state.get("team_owner").cloned());
    }
    let key = team_state_key(state);
    let caller = caller_identity_from_env(Some(state), env, Some(&key), None)?;
    if caller.pane_id.is_empty() {
        return Ok(None);
    }
    // 键序对齐 Python dict 字面。
    let owner = json!({
        "pane_id": caller.pane_id,
        "provider": caller.provider,
        "machine_fingerprint": caller.machine_fingerprint,
        "leader_session_uuid": caller.leader_session_uuid,
        "claimed_at": now_iso,
        "claimed_via": source,
    });
    if let Some(o) = state.as_object_mut() {
        o.insert("team_owner".to_string(), owner.clone());
    }
    Ok(Some(owner))
}

/// `apply_first_time_leader_binding`(`state.py:434`):命令 + workspace realpath 双门后,
/// 写入 team_owner / leader_receiver(owner_epoch=0)。拒绝 dict 字节对齐 Python(含 `pane` +
/// `repr()` 错误串)。`command_looks_usable`(`_leader_command_looks_usable`,step 11)经闭包注入。
#[allow(clippy::too_many_arguments)]
pub fn apply_first_time_leader_binding(
    workspace: &Path,
    state: &mut Value,
    receiver: &mut Value,
    pane_info: &Value,
    identity: &Value,
    source: &str,
    now_iso: &str,
    command_looks_usable: impl Fn(&str, &str) -> bool,
) -> Value {
    let command = pane_info.get("pane_current_command").and_then(Value::as_str).unwrap_or("");
    // str(receiver.get("provider") or "")
    let provider = receiver.get("provider").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("").to_string();
    if !command_looks_usable(command, &provider) {
        return json!({
            "ok": false,
            "reason": "leader_pane_wrong_command",
            "error": format!("pane command {} is not a leader host", py_repr_str(command)),
            "pane": pane_info,
        });
    }
    // 比较:realpath(current_path) == realpath(workspace.resolve());消息:repr(str(workspace.resolve()))。
    let ws_resolved = realpath_like(workspace);
    let current_path = pane_info.get("pane_current_path").and_then(Value::as_str);
    let cwd_ok = current_path
        .filter(|p| !p.is_empty())
        .is_some_and(|p| realpath_like(Path::new(p)) == ws_resolved);
    if !cwd_ok {
        let cur_repr = match current_path {
            Some(p) => py_repr_str(p),
            None => "None".to_string(),
        };
        return json!({
            "ok": false,
            "reason": "leader_pane_wrong_workspace",
            "error": format!(
                "pane cwd {} does not match workspace {}",
                cur_repr,
                py_repr_str(&path_str(&ws_resolved))
            ),
            "pane": pane_info,
        });
    }
    // receiver.update({leader_session_uuid, machine_fingerprint, owner_epoch: 0})
    let id_uuid = identity.get("leader_session_uuid").cloned().unwrap_or(Value::Null);
    let id_fp = identity.get("machine_fingerprint").cloned().unwrap_or(Value::Null);
    if let Some(r) = receiver.as_object_mut() {
        r.insert("leader_session_uuid".to_string(), id_uuid.clone());
        r.insert("machine_fingerprint".to_string(), id_fp.clone());
        r.insert("owner_epoch".to_string(), json!(0));
        if let Some(socket) = crate::tmux_backend::socket_name_from_tmux_env() {
            r.insert("tmux_socket".to_string(), json!(socket));
        }
    }
    let owner = json!({
        "pane_id": receiver.get("pane_id").cloned().unwrap_or(Value::Null),
        "provider": provider,
        "machine_fingerprint": id_fp,
        "leader_session_uuid": id_uuid,
        "owner_epoch": 0,
        "claimed_at": now_iso,
        "claimed_via": source,
    });
    if let Some(o) = state.as_object_mut() {
        o.insert("team_owner".to_string(), owner);
        o.insert("leader_receiver".to_string(), receiver.clone());
    }
    json!({ "ok": true, "pane": pane_info, "warning": null, "first_time": true })
}

/// `leader_env_exports`(`state.py:469`):receiver + identity → 6 个 leader env 导出(插入序)。
pub fn leader_env_exports(receiver: &Value, identity: &Value) -> Vec<(String, String)> {
    vec![
        ("TEAM_AGENT_LEADER_PANE_ID".to_string(), str_or_empty(receiver.get("pane_id"))),
        ("TEAM_AGENT_LEADER_PROVIDER".to_string(), str_or_empty(receiver.get("provider"))),
        ("TEAM_AGENT_LEADER_SESSION_UUID".to_string(), str_or_empty(identity.get("leader_session_uuid"))),
        ("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), str_or_empty(identity.get("machine_fingerprint"))),
        ("TEAM_AGENT_WORKSPACE".to_string(), str_or_empty(identity.get("workspace_abspath"))),
        ("TEAM_AGENT_TEAM_ID".to_string(), str_or_empty(identity.get("team_id"))),
    ]
}

/// `validate_leader_uuid_from_targets`(`state.py:480`):fake provider 直过;否则核对 targets
/// 扫描成功 + receiver.pane_id 在 targets 中存在。
pub fn validate_leader_uuid_from_targets(receiver: &Value, targets: &Value) -> Value {
    if receiver.get("provider").and_then(Value::as_str) == Some("fake") {
        return json!({ "ok": true });
    }
    if !targets.get("ok").is_some_and(json_truthy) {
        let error = targets
            .get("error")
            .filter(|v| json_truthy(v))
            .cloned()
            .unwrap_or_else(|| json!("tmux target scan failed"));
        return json!({ "ok": false, "reason": "leader_uuid_lookup_failed", "error": error });
    }
    let pane_id = receiver.get("pane_id");
    let target = targets
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|arr| arr.iter().find(|item| item.get("pane_id") == pane_id));
    match target {
        Some(t) => json!({ "ok": true, "pane": t }),
        None => json!({ "ok": false, "reason": "leader_pane_missing", "error": "tmux pane does not exist" }),
    }
}

// ---- helpers ----

/// `str(v or "")`(string 域):truthy 字符串 → 自身,否则 `""`。
fn str_or_empty(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("").to_string()
}

/// Python `repr()` of a `str`(与 `model::spec::py_repr_str` 同实现)。
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
    use serial_test::serial;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    struct MockEnv(HashMap<String, String>);
    impl IdentityEnv for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }
    fn env(pairs: &[(&str, &str)]) -> MockEnv {
        MockEnv(pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect())
    }
    fn same_repr(a: &Value, b: &Value) {
        assert_eq!(serde_json::to_string(a).unwrap(), serde_json::to_string(b).unwrap());
    }
    fn temp_ws() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!("ta_rs_id_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    struct EnvUnsetGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }
    impl EnvUnsetGuard {
        fn unset(keys: &[&'static str]) -> Self {
            let previous = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }
    impl Drop for EnvUnsetGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..).rev() {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn os_user_and_fingerprint() {
        assert_eq!(identity_os_user(&env(&[("USER", "alice")])), "alice");
        assert_eq!(identity_os_user(&env(&[("USERNAME", "bob")])), "bob");
        assert_eq!(identity_os_user(&env(&[])), "");
        // state record fp 优先 env。
        let s = json!({"team_owner": {"machine_fingerprint": "from-owner"}});
        assert_eq!(identity_machine_fingerprint(&s, &env(&[("TEAM_AGENT_MACHINE_FINGERPRINT", "envfp")])), "from-owner");
        assert_eq!(identity_machine_fingerprint(&json!({}), &env(&[("TEAM_AGENT_MACHINE_FINGERPRINT", "envfp")])), "envfp");
    }

    #[test]
    fn caller_identity_derived_env_override_golden() {
        let base = [
            ("TEAM_AGENT_MACHINE_FINGERPRINT", "fp1"),
            ("TEAM_AGENT_WORKSPACE", "/ws/proj"),
            ("USER", "alice"),
            ("TEAM_AGENT_LEADER_PANE_ID", "%5"),
            ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
        ];
        // derived(golden uuid 由 Python 真相源算出;/ws/proj 两机皆不存在 → realpath 词法等价)。
        let cid = caller_identity_from_env(Some(&json!({})), &env(&base), Some("teamA"), None).unwrap();
        assert_eq!(cid.leader_session_uuid, "379f5e361ec429edabea7022391e8ca8");
        assert_eq!(cid.leader_session_uuid_source, "derived");
        assert_eq!(cid.pane_id, "%5");
        assert_eq!(cid.provider, "codex");
        assert_eq!(cid.machine_fingerprint, "fp1");

        let mut env_v: Vec<(&str, &str)> = base.to_vec();
        env_v.push(("TEAM_AGENT_LEADER_SESSION_UUID", "envuuid123"));
        let cid_env = caller_identity_from_env(Some(&json!({})), &env(&env_v), Some("teamA"), None).unwrap();
        assert_eq!(cid_env.leader_session_uuid, "envuuid123");
        assert_eq!(cid_env.leader_session_uuid_source, "env");

        env_v.push(("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", "ovr999"));
        let cid_ovr = caller_identity_from_env(Some(&json!({})), &env(&env_v), Some("teamA"), None).unwrap();
        assert_eq!(cid_ovr.leader_session_uuid, "ovr999");
        assert_eq!(cid_ovr.leader_session_uuid_source, "explicit-override");
    }

    #[test]
    fn pane_id_falls_back_to_tmux_pane() {
        let cid = caller_identity_from_env(
            Some(&json!({})),
            &env(&[("TMUX_PANE", "%tmux"), ("TEAM_AGENT_LEADER_SESSION_UUID", "u")]),
            Some("t"),
            None,
        )
        .unwrap();
        assert_eq!(cid.pane_id, "%tmux");
    }

    #[test]
    fn migrate_team_identity_fills_then_idempotent() {
        let e = env(&[("USER", "u"), ("TEAM_AGENT_MACHINE_FINGERPRINT", "fp")]);
        let mut state = json!({"team_owner": {"pane_id": "%1"}, "session_name": "s"});
        let changed = migrate_team_identity(&mut state, &e, Some(Path::new("/ws")), Some("tid")).unwrap();
        assert!(changed);
        let uuid = state["team_owner"]["leader_session_uuid"].as_str().unwrap().to_string();
        // 与 derive 公式一致(workspace=/ws 不存在 → 词法 "/ws")。
        let expected = LeaderSessionUuid::derive("fp", "/ws", "u", "tid").unwrap().as_str().to_string();
        assert_eq!(uuid, expected);
        // 二次调用:已有非空 uuid → 不改。
        assert!(!migrate_team_identity(&mut state, &e, Some(Path::new("/ws")), Some("tid")).unwrap());
    }

    #[test]
    fn migrate_state_identity_top_and_per_team() {
        let e = env(&[("USER", "u"), ("TEAM_AGENT_MACHINE_FINGERPRINT", "fp")]);
        let mut state = json!({
            "session_name": "s",
            "team_owner": {"pane_id": "%1"},
            "teams": {"t1": {"team_owner": {"pane_id": "%2"}}, "t2": {"session_name": "x"}},
        });
        let changed = migrate_state_identity(&mut state, &e, Path::new("/ws")).unwrap();
        assert!(changed);
        assert!(state["team_owner"]["leader_session_uuid"].as_str().unwrap().len() == 32);
        assert!(state["teams"]["t1"]["team_owner"]["leader_session_uuid"].as_str().unwrap().len() == 32);
        // t1 的 uuid 用 team_id="t1" 派生,异于顶层(team_id=team_state_key=session "s")。
        assert_ne!(
            state["team_owner"]["leader_session_uuid"],
            state["teams"]["t1"]["team_owner"]["leader_session_uuid"]
        );
    }

    #[test]
    fn populate_team_owner_seed_and_skip() {
        let e_with_pane = env(&[
            ("TEAM_AGENT_LEADER_PANE_ID", "%9"),
            ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
            ("TEAM_AGENT_MACHINE_FINGERPRINT", "fp"),
            ("TEAM_AGENT_LEADER_SESSION_UUID", "u1"),
        ]);
        let mut state = json!({});
        let owner = populate_team_owner_from_env(&mut state, "autopopulate", &e_with_pane, "TS").unwrap().unwrap();
        same_repr(
            &owner,
            &json!({"pane_id": "%9", "provider": "codex", "machine_fingerprint": "fp",
                    "leader_session_uuid": "u1", "claimed_at": "TS", "claimed_via": "autopopulate"}),
        );
        same_repr(&state["team_owner"], &owner);

        // 无 pane_id → None,不写 owner。
        let mut state2 = json!({});
        assert!(populate_team_owner_from_env(&mut state2, "x", &env(&[]), "TS").unwrap().is_none());
        assert!(state2.get("team_owner").is_none());

        // 已有 team_owner(缺 uuid)→ 迁移补 uuid 后返回。
        let mut state3 = json!({"team_owner": {"pane_id": "%1", "machine_fingerprint": "fp"}, "session_name": "s"});
        let o3 = populate_team_owner_from_env(&mut state3, "x", &env(&[("USER", "u")]), "TS").unwrap().unwrap();
        assert_eq!(o3["leader_session_uuid"].as_str().unwrap().len(), 32);
    }

    #[test]
    #[serial(env)]
    fn apply_first_time_binding_success_writes_state() {
        let _env = EnvUnsetGuard::unset(&["TMUX", "TMUX_PANE"]);
        let ws = temp_ws();
        let now = "2026-06-02T09:17:59.994383+00:00";
        let mut state = json!({});
        let mut receiver = json!({"pane_id": "%9", "provider": "codex"});
        let identity = json!({"leader_session_uuid": "uuidX", "machine_fingerprint": "fpX"});
        let pane = json!({"pane_current_command": "codex", "pane_current_path": ws.to_string_lossy()});
        let r = apply_first_time_leader_binding(&ws, &mut state, &mut receiver, &pane, &identity, "claim", now, |_c, _p| true);
        same_repr(&r, &json!({"ok": true, "pane": pane, "warning": null, "first_time": true}));
        same_repr(
            &state["team_owner"],
            &json!({"pane_id": "%9", "provider": "codex", "machine_fingerprint": "fpX",
                    "leader_session_uuid": "uuidX", "owner_epoch": 0, "claimed_at": now, "claimed_via": "claim"}),
        );
        same_repr(
            &state["leader_receiver"],
            &json!({"pane_id": "%9", "provider": "codex", "leader_session_uuid": "uuidX",
                    "machine_fingerprint": "fpX", "owner_epoch": 0}),
        );
    }

    #[test]
    fn apply_first_time_binding_refusals() {
        let ws = temp_ws();
        let now = "TS";
        let identity = json!({"leader_session_uuid": "uuidX", "machine_fingerprint": "fpX"});
        // wrong command(闭包返回 false)。
        let mut s1 = json!({});
        let mut r1 = json!({"pane_id": "%9", "provider": "codex"});
        let pane1 = json!({"pane_current_command": "bash", "pane_current_path": ws.to_string_lossy()});
        let res1 = apply_first_time_leader_binding(&ws, &mut s1, &mut r1, &pane1, &identity, "claim", now, |_c, _p| false);
        assert_eq!(res1["ok"], json!(false));
        assert_eq!(res1["reason"], json!("leader_pane_wrong_command"));
        assert_eq!(res1["error"], json!("pane command 'bash' is not a leader host"));
        assert_eq!(res1["pane"], pane1);
        assert!(s1.get("team_owner").is_none(), "拒绝不得写 state");

        // wrong workspace。
        let mut s2 = json!({});
        let mut r2 = json!({"pane_id": "%9", "provider": "codex"});
        let pane2 = json!({"pane_current_command": "codex", "pane_current_path": "/nowhere/else"});
        let res2 = apply_first_time_leader_binding(&ws, &mut s2, &mut r2, &pane2, &identity, "claim", now, |_c, _p| true);
        assert_eq!(res2["reason"], json!("leader_pane_wrong_workspace"));
        let ws_repr = py_repr_str(&path_str(&realpath_like(&ws)));
        assert_eq!(
            res2["error"],
            json!(format!("pane cwd '/nowhere/else' does not match workspace {ws_repr}"))
        );
        assert!(s2.get("team_owner").is_none());
    }

    #[test]
    fn leader_env_exports_golden() {
        let exports = leader_env_exports(
            &json!({"pane_id": "%1", "provider": "codex"}),
            &json!({"leader_session_uuid": "u1", "machine_fingerprint": "f1", "workspace_abspath": "/w", "team_id": "t"}),
        );
        assert_eq!(
            exports,
            vec![
                ("TEAM_AGENT_LEADER_PANE_ID".to_string(), "%1".to_string()),
                ("TEAM_AGENT_LEADER_PROVIDER".to_string(), "codex".to_string()),
                ("TEAM_AGENT_LEADER_SESSION_UUID".to_string(), "u1".to_string()),
                ("TEAM_AGENT_MACHINE_FINGERPRINT".to_string(), "f1".to_string()),
                ("TEAM_AGENT_WORKSPACE".to_string(), "/w".to_string()),
                ("TEAM_AGENT_TEAM_ID".to_string(), "t".to_string()),
            ]
        );
    }

    #[test]
    fn validate_leader_uuid_from_targets_cases() {
        same_repr(&validate_leader_uuid_from_targets(&json!({"provider": "fake"}), &json!({})), &json!({"ok": true}));
        same_repr(
            &validate_leader_uuid_from_targets(&json!({"provider": "codex"}), &json!({"ok": false, "error": "boom"})),
            &json!({"ok": false, "reason": "leader_uuid_lookup_failed", "error": "boom"}),
        );
        same_repr(
            &validate_leader_uuid_from_targets(&json!({"provider": "codex"}), &json!({"ok": false})),
            &json!({"ok": false, "reason": "leader_uuid_lookup_failed", "error": "tmux target scan failed"}),
        );
        same_repr(
            &validate_leader_uuid_from_targets(
                &json!({"provider": "codex", "pane_id": "%1"}),
                &json!({"ok": true, "targets": [{"pane_id": "%2"}]}),
            ),
            &json!({"ok": false, "reason": "leader_pane_missing", "error": "tmux pane does not exist"}),
        );
        same_repr(
            &validate_leader_uuid_from_targets(
                &json!({"provider": "codex", "pane_id": "%1"}),
                &json!({"ok": true, "targets": [{"pane_id": "%1", "x": 1}]}),
            ),
            &json!({"ok": true, "pane": {"pane_id": "%1", "x": 1}}),
        );
    }
}
