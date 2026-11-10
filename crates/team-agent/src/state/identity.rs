//!
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

///
/// `_identity_os_user`(`state.py:275`):`USER` or `USERNAME` or `""`。
fn identity_os_user(env: &dyn IdentityEnv) -> String {
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

///
/// `_identity_machine_fingerprint`(`state.py:279`):team_owner/leader_receiver 的非空
/// machine_fingerprint 优先,否则 env `TEAM_AGENT_MACHINE_FINGERPRINT` or `""`。
fn identity_machine_fingerprint(state: &Value, env: &dyn IdentityEnv) -> String {
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

///
/// `_identity_workspace_abspath`(`state.py:264`):按 workspace→team_dir→spec_path→fallback
/// 顺序派生 workspace 绝对路径(各分支先 `resolve()` 再做 parent 导航)。
fn identity_workspace_abspath(
    state: &Value,
    env: &dyn IdentityEnv,
    workspace: Option<&Path>,
) -> String {
    if let Some(ws) = state
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return path_str(&realpath_like(Path::new(ws)));
    }
    if let Some(td) = state
        .get("team_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        // resolve().parent.parent
        let r = realpath_like(Path::new(td));
        let pp = r
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| r.clone(), Path::to_path_buf);
        return path_str(&pp);
    }
    if let Some(sp) = state
        .get("spec_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let r = realpath_like(Path::new(sp));
        let parent = r.parent();
        let grandparent = parent.and_then(Path::parent);
        // grandparent.name == ".team" ? grandparent.parent : parent
        let result = if grandparent
            .and_then(Path::file_name)
            .is_some_and(|n| n == std::ffi::OsStr::new(".team"))
        {
            grandparent
                .and_then(Path::parent)
                .map_or_else(|| r.clone(), Path::to_path_buf)
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

///
/// `_migrate_team_identity`(`state.py:295`):为缺 `leader_session_uuid` 的 team_owner /
/// leader_receiver 填入派生 uuid。返回是否有改动。
fn migrate_team_identity(
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
            Some(r) => r
                .get("leader_session_uuid")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty),
            None => false,
        };
        if needs {
            if let Some(obj) = state.get_mut(key).and_then(Value::as_object_mut) {
                obj.insert(
                    "leader_session_uuid".to_string(),
                    Value::String(leader_uuid.clone()),
                );
                changed = true;
            }
        }
    }
    Ok(changed)
}

///
/// `_migrate_state_identity`(`state.py:306`):顶层(有 session_name 时)+ 每个 team 子状态迁移。
pub fn migrate_state_identity(
    state: &mut Value,
    env: &dyn IdentityEnv,
    workspace: &Path,
) -> Result<bool, ModelError> {
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
        if let Some(team_state) = state
            .get_mut("teams")
            .and_then(Value::as_object_mut)
            .and_then(|t| t.get_mut(&tid))
        {
            if team_state.is_object() {
                let c = migrate_team_identity(team_state, env, Some(workspace), Some(&tid))?;
                changed = c || changed;
            }
        }
    }
    Ok(changed)
}

///
/// `_caller_identity_from_env`(`state.py:316`):env override/env/derived + caller tmux endpoint。
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
        tmux_socket: env.var("TMUX").and_then(|value| {
            value.split(',').next().filter(|socket| !socket.is_empty()).map(str::to_owned)
        }),
        provider: env_str(env, "TEAM_AGENT_LEADER_PROVIDER"),
        machine_fingerprint,
        leader_session_uuid: leader_uuid,
        leader_session_uuid_source: source.to_string(),
    })
}





// ---- helpers ----



#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use serial_test::serial;
    use std::collections::HashMap;

    struct MockEnv(HashMap<String, String>);
    impl IdentityEnv for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }
    fn env(pairs: &[(&str, &str)]) -> MockEnv {
        MockEnv(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }
    fn same_repr(a: &Value, b: &Value) {
        assert_eq!(
            serde_json::to_string(a).unwrap(),
            serde_json::to_string(b).unwrap()
        );
    }

    #[test]
    fn os_user_and_fingerprint() {
        assert_eq!(identity_os_user(&env(&[("USER", "alice")])), "alice");
        assert_eq!(identity_os_user(&env(&[("USERNAME", "bob")])), "bob");
        assert_eq!(identity_os_user(&env(&[])), "");
        // state record fp 优先 env。
        let s = json!({"team_owner": {"machine_fingerprint": "from-owner"}});
        assert_eq!(
            identity_machine_fingerprint(&s, &env(&[("TEAM_AGENT_MACHINE_FINGERPRINT", "envfp")])),
            "from-owner"
        );
        assert_eq!(
            identity_machine_fingerprint(
                &json!({}),
                &env(&[("TEAM_AGENT_MACHINE_FINGERPRINT", "envfp")])
            ),
            "envfp"
        );
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
        let cid =
            caller_identity_from_env(Some(&json!({})), &env(&base), Some("teamA"), None).unwrap();
        assert_eq!(cid.leader_session_uuid, "379f5e361ec429edabea7022391e8ca8");
        assert_eq!(cid.leader_session_uuid_source, "derived");
        assert_eq!(cid.pane_id, "%5");
        assert_eq!(cid.provider, "codex");
        assert_eq!(cid.machine_fingerprint, "fp1");

        let mut env_v: Vec<(&str, &str)> = base.to_vec();
        env_v.push(("TEAM_AGENT_LEADER_SESSION_UUID", "envuuid123"));
        let cid_env =
            caller_identity_from_env(Some(&json!({})), &env(&env_v), Some("teamA"), None).unwrap();
        assert_eq!(cid_env.leader_session_uuid, "envuuid123");
        assert_eq!(cid_env.leader_session_uuid_source, "env");

        env_v.push(("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", "ovr999"));
        let cid_ovr =
            caller_identity_from_env(Some(&json!({})), &env(&env_v), Some("teamA"), None).unwrap();
        assert_eq!(cid_ovr.leader_session_uuid, "ovr999");
        assert_eq!(cid_ovr.leader_session_uuid_source, "explicit-override");
    }

    #[test]
    fn pane_id_falls_back_to_tmux_pane() {
        let cid = caller_identity_from_env(
            Some(&json!({})),
            &env(&[
                ("TMUX_PANE", "%tmux"),
                ("TMUX", "/tmp/caller.sock,123,0"),
                ("TEAM_AGENT_LEADER_SESSION_UUID", "u"),
            ]),
            Some("t"),
            None,
        )
        .unwrap();
        assert_eq!(cid.pane_id, "%tmux");
        assert_eq!(cid.tmux_socket.as_deref(), Some("/tmp/caller.sock"));
    }

    #[test]
    fn migrate_team_identity_fills_then_idempotent() {
        let e = env(&[("USER", "u"), ("TEAM_AGENT_MACHINE_FINGERPRINT", "fp")]);
        let mut state = json!({"team_owner": {"pane_id": "%1"}, "session_name": "s"});
        let changed =
            migrate_team_identity(&mut state, &e, Some(Path::new("/ws")), Some("tid")).unwrap();
        assert!(changed);
        let uuid = state["team_owner"]["leader_session_uuid"]
            .as_str()
            .unwrap()
            .to_string();
        // 与 derive 公式一致(workspace=/ws 不存在 → 词法 "/ws")。
        let expected = LeaderSessionUuid::derive("fp", "/ws", "u", "tid")
            .unwrap()
            .as_str()
            .to_string();
        assert_eq!(uuid, expected);
        // 二次调用:已有非空 uuid → 不改。
        assert!(
            !migrate_team_identity(&mut state, &e, Some(Path::new("/ws")), Some("tid")).unwrap()
        );
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
        assert!(
            state["team_owner"]["leader_session_uuid"]
                .as_str()
                .unwrap()
                .len()
                == 32
        );
        assert!(
            state["teams"]["t1"]["team_owner"]["leader_session_uuid"]
                .as_str()
                .unwrap()
                .len()
                == 32
        );
        // t1 的 uuid 用 team_id="t1" 派生,异于顶层(team_id=team_state_key=session "s")。
        assert_ne!(
            state["team_owner"]["leader_session_uuid"],
            state["teams"]["t1"]["team_owner"]["leader_session_uuid"]
        );
    }

    #[test]
    fn populate_team_owner_seed_and_skip() {
        let mut state = json!({});
        let owner = json!({"pane_id": "%9", "provider": "codex", "machine_fingerprint": "fp",
            "leader_session_uuid": "u1", "claimed_at": "TS", "claimed_via": "autopopulate"});
        let team_key = crate::state::projection::team_state_key(&state);
        crate::state::ownership::write_owner(
            &mut state, &team_key,
            crate::state::ownership::OwnershipWrite::new().with_team_owner(owner.clone()),
        );
        let repo_owner = crate::state::ownership::read_owner_value(&state, &team_key)
            .expect("owner readable via canonical ownership repository");
        same_repr(repo_owner, &owner);

        let state2 = json!({});
        let team_key2 = crate::state::projection::team_state_key(&state2);
        assert!(crate::state::ownership::read_owner_value(&state2, &team_key2).is_none());
    }

    #[test]
    #[serial(env)]
    fn apply_first_time_binding_success_writes_state() {
        let mut state = json!({});
        let now = "2026-06-02T09:17:59.994383+00:00";
        let owner = json!({"pane_id": "%9", "provider": "codex", "machine_fingerprint": "fpX",
            "leader_session_uuid": "uuidX", "owner_epoch": 0, "claimed_at": now, "claimed_via": "claim"});
        let receiver = json!({"pane_id": "%9", "provider": "codex", "leader_session_uuid": "uuidX",
            "machine_fingerprint": "fpX", "owner_epoch": 0});
        let team_key = crate::state::projection::team_state_key(&state);
        crate::state::ownership::write_owner(
            &mut state, &team_key,
            crate::state::ownership::OwnershipWrite::new()
                .with_team_owner(owner.clone()).with_leader_receiver(receiver.clone()).with_owner_epoch(0),
        );
        let team_owner = crate::state::ownership::read_owner_value(&state, &team_key)
            .expect("team_owner reachable via canonical repository");
        same_repr(team_owner, &owner);
        same_repr(&state["teams"][&team_key]["leader_receiver"], &receiver);
    }



}
