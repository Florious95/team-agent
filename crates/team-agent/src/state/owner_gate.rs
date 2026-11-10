//!
//! owner-gate:trust own-vs-foreign(§11 / bug-064 / bug-082 血泪;真相源 `state.py`)。
//!
//! 三核心纯判定(集成 send_message/MCP 是 step 11/14):
//! - `check_team_owner`:pane-id / uuid / **tmux-liveness** 身份门(pane-as-identity 模型)。
//!   liveness 依赖经 [`PaneLiveness`] trait 注入,真 tmux 探测延 step 9。
//! - `apply_first_time_leader_binding` 的 **realpath own-vs-foreign**(`state.py:449`):
//!   `canonicalize(pane_cwd) == canonicalize(workspace)` —— **两边都 canonical 化**,这正是
//!   macOS `/tmp → /private/tmp` 软链不对称(0.2.11 真机抓出、单测漏掉的 bug)的正解。
//!   **禁 basename / startswith / 子串 / 反推 cwd**(§11)。
//! - `worker_sender_bypasses_owner_gate`:worker peer-send 绕过 owner 门(sender 白名单 + env 一致)。
//!
//! own→自动答(本地 tmux 机械动作,MUST-NOT-13 零 provider 调用)+ trust 截断匹配是 **step 11**
//! messaging delivery(`attempt_trust_auto_answer` / `_tmux_pane_width`),不在此 slice。
//!
//! §10:纯判定无 unwrap/expect/panic;realpath 比对 canonicalize 失败时退化为词法绝对路径比较。

use std::path::Path;

use serde_json::{json, Value};

use crate::model::enums::PaneLiveness;

/// tmux pane 存活探测(`state.py:_tmux_pane_liveness`)。真实现走 tmux(step 9 transport);
/// owner-gate 单测用 mock 注入。
pub trait PaneLivenessProbe {
    fn liveness(&self, pane_id: &str) -> PaneLiveness;
}

/// 调用方身份(`_caller_identity_from_env` 的产物;此处作为注入参数保持判定纯净)。
#[derive(Debug, Clone, Default)]
pub struct CallerIdentity {
    pub pane_id: String,
    pub tmux_socket: Option<String>,
    pub provider: String,
    pub machine_fingerprint: String,
    pub leader_session_uuid: String,
    /// `explicit-override` | `env` | `derived`(Python caller dict 的第 5 字段)。
    pub leader_session_uuid_source: String,
}

///
/// `check_team_owner`(`state.py:366`)的纯判定版:caller 身份 + 是否有 TEAM_AGENT_ID + liveness 注入。
/// 返回 `None`=允许(own / 无 owner / 死 owner pane 可接管);`Some(dict)`=拒绝(team_owner_mismatch)。
///
/// Stage 2 (identity-boundary unified plan, architect direction 2026-06-23):
/// the owner is now looked up through `state::ownership::read_owner_value`
/// instead of `state::projection::read_owner` directly. The two are
/// equivalent today for the gate's `None` (empty team_key) path — both
/// resolve to top-level `state.team_owner` — but routing through the
/// repository means Stage 5 can swap the data source (per-team canonical
/// `state.json`) without touching this gate. The empty-team-key argument
/// preserves the pre-Stage-2 shape: gate callers feed an
/// already-team-projected state and expect top-level reads.
pub fn check_team_owner(
    state: &Value,
    caller: &CallerIdentity,
    has_team_agent_id: bool,
    liveness: &dyn PaneLivenessProbe,
) -> Option<Value> {
    let owner = crate::state::ownership::read_owner_value(state, "")?;
    if !owner.is_object() || owner.as_object().is_none_or(serde_json::Map::is_empty) {
        return None;
    }
    let owner_uuid = owner
        .get("leader_session_uuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    let owner_pane = owner.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let caller_uuid = caller.leader_session_uuid.as_str();
    let caller_pane = caller.pane_id.as_str();

    let owner_socket = owner.get("tmux_socket").and_then(Value::as_str)
        .filter(|socket| !socket.is_empty())
        .or_else(|| state.pointer("/leader_receiver/tmux_socket").and_then(Value::as_str)
            .filter(|socket| !socket.is_empty()));
    // Pane IDs are server-local; only legacy owners without an endpoint use pane-only identity.
    let same_socket = owner_socket.is_none_or(|owner_socket| {
        caller.tmux_socket.as_deref().filter(|socket| !socket.is_empty())
            .is_some_and(|socket| crate::topology::same_endpoint(owner_socket, socket))
    });
    let same_pane = same_socket && !caller_pane.is_empty() && caller_pane == owner_pane;
    if same_pane {
        return None;
    }
    // 死 owner pane + 新 live caller(非 worker)→ 接管(pane-as-identity:死 pane 不锁活 caller)。
    if same_socket
        && !caller_pane.is_empty()
        && !has_team_agent_id
        && !owner_pane.is_empty()
        && liveness.liveness(owner_pane) != PaneLiveness::Live
    {
        return None;
    }
    // 同 uuid 且(无 caller_pane 或同 pane)→ own。
    // 安全守卫(对抗 P0-A):**两边非空**才算同 uuid —— 堵住 owner_uuid=="" && caller_uuid=="" 的
    // "" == "" allow-flip(否则缺 uuid 的 owner 会把外来空-uuid caller 误判为 own)。
    // 注(对抗 verifier 确认的 P2 parity gap,安全侧):Python check_team_owner 开头调
    // _migrate_team_identity 把缺 uuid 的 owner 填成 derived(故其 owner_uuid 恒非空);该迁移需
    // env+workspace+可变 state,而本纯判定签名(`&Value` + 注入 liveness)尚不持有这些。
    // identity::migrate_team_identity 已实现,但 check_team_owner 的实际 caller(env/workspace 来源)
    // 在 step 11 messaging 才落地 —— 届时在此先 migrate owner identity 再判定,收口该 gap。
    // 在此之前本守卫**偏严**:缺-uuid owner + 同-identity 异-pane caller 会被判 takeover(Python 判
    // sticky/own)。**只过严不过松**(宁拒不误放),无安全洞。
    let same_uuid = !owner_uuid.is_empty() && !caller_uuid.is_empty() && caller_uuid == owner_uuid;
    if same_socket && same_uuid && (caller_pane.is_empty() || same_pane) {
        return None;
    }
    // 否则拒绝。reason_kind / action 按 same_uuid 分流(sticky_bind vs takeover)。
    let (reason_kind, action) = if same_uuid {
        ("sticky_bind_collision", "team-agent claim-leader --confirm")
    } else {
        ("owner_takeover_required", "team-agent claim-leader --confirm")
    };
    // 键序对齐 Python dict 字面(preserve_order)。
    Some(json!({
        "ok": false,
        "status": "refused",
        "reason": "team_owner_mismatch",
        "reason_kind": reason_kind,
        "error": "not_owner",
        "action": action,
        "team_owner": owner.clone(),
        "caller": caller_to_value(caller),
    }))
}

fn caller_to_value(c: &CallerIdentity) -> Value {
    // 字段集对齐 Python `_caller_identity_from_env`(5 键含 leader_session_uuid_source)。
    json!({
        "pane_id": c.pane_id,
        "provider": c.provider,
        "machine_fingerprint": c.machine_fingerprint,
        "leader_session_uuid": c.leader_session_uuid,
        "leader_session_uuid_source": c.leader_session_uuid_source,
    })
}

/// Follow only persisted parent keys; never infer family membership from names.
pub(crate) fn team_ancestors(state: &Value, team: &str) -> Result<Vec<String>, String> {
    let mut ancestors = Vec::new();
    let mut seen = std::collections::BTreeSet::from([team.to_string()]);
    let mut current = team;
    while let Some(parent) = state["teams"][current]["parent_team_key"].as_str().filter(|p| !p.is_empty()) {
        if !seen.insert(parent.to_string()) || !state["teams"][parent].is_object() {
            return Err("invalid team parent chain".into());
        }
        ancestors.push(parent.to_string());
        current = parent;
    }
    Ok(ancestors)
}

/// Stop-only delegation: an actual live owner of an ancestor, on the caller's server.
pub(crate) fn ancestor_owner_allowed(
    state: &Value, target: &str, caller: &CallerIdentity, endpoint: &str,
    observed_pane: &str, probe: &dyn PaneLivenessProbe,
) -> bool {
    if caller.pane_id != observed_pane { return false; }
    let Ok(ancestors) = team_ancestors(state, target) else { return false; };
    ancestors.iter().any(|key| {
        let ancestor = &state["teams"][key];
        let owner = &ancestor["team_owner"];
        let socket = owner["tmux_socket"].as_str()
            .or_else(|| ancestor["leader_receiver"]["tmux_socket"].as_str());
        !caller.pane_id.is_empty()
            && !ancestor["status"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("shutdown"))
            && owner["pane_id"].as_str() == Some(caller.pane_id.as_str())
            && socket.is_some_and(|socket| crate::topology::same_endpoint(socket, endpoint))
            && probe.liveness(&caller.pane_id) == PaneLiveness::Live
            && check_team_owner(ancestor, caller, false, probe).is_none()
    })
}



fn lexical_abs(p: &Path) -> std::path::PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

///
/// `os.path.realpath` 等价(对抗 P0-B):全路径存在 → `canonicalize`;否则对**最长存在前缀**
/// canonicalize(解析其软链)再拼回缺失末段(规范化 `..`/`.`)。这复刻 Python「解析存在部分的
/// 软链 + 保留不存在末段」,修掉「父目录是软链 + 末段不存在 → 漏判 own」的 §11 血泪。
pub(crate) fn realpath_like(p: &Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};
    if let Ok(c) = std::fs::canonicalize(p) {
        return c; // 全路径存在(canonicalize 已解析所有软链 + 规范化 ..)
    }
    let abs = lexical_abs(p);
    let comps: Vec<Component> = abs.components().collect();
    // 从最长前缀往短试,找第一个可 canonicalize 的存在前缀。
    for split in (1..=comps.len()).rev() {
        let prefix: PathBuf = comps[..split].iter().collect();
        if prefix.as_os_str().is_empty() {
            continue;
        }
        if let Ok(canon) = std::fs::canonicalize(&prefix) {
            let mut result = canon;
            for comp in &comps[split..] {
                match comp {
                    Component::ParentDir => {
                        result.pop();
                    }
                    Component::Normal(n) => result.push(n),
                    Component::CurDir => {}
                    Component::RootDir | Component::Prefix(_) => {}
                }
            }
            return result;
        }
    }
    abs // 无任何存在前缀(极少)→ 词法绝对(已含 lexical 形)
}

// 注:`apply_first_time_leader_binding` 的 realpath+命令双门由 `identity::apply_first_time_leader_binding`
// 字节对齐实现(含 `pane` 字段 + `repr()` 错误串)。早期此处曾有一个简化抽取 `first_time_binding_gate`
// (拒绝串用 `{:?}` 非 repr、缺 `pane`/workspace 字段),与 Python 不对拍且无生产 caller —— 对抗 verifier
// 确认为冗余分歧陷阱,已删除。`workspace_paths_match`/`realpath_like` 作为 §11 realpath 原语保留
// (step 11 messaging trust auto-answer 复用)。

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    struct MockLiveness(PaneLiveness);
    impl PaneLivenessProbe for MockLiveness {
        fn liveness(&self, _pane_id: &str) -> PaneLiveness {
            self.0
        }
    }
    fn caller(pane: &str, uuid: &str) -> CallerIdentity {
        CallerIdentity {
            pane_id: pane.to_string(),
            tmux_socket: None,
            provider: "codex".to_string(),
            machine_fingerprint: "m".to_string(),
            leader_session_uuid: uuid.to_string(),
            leader_session_uuid_source: "derived".to_string(),
        }
    }
    const LIVE: MockLiveness = MockLiveness(PaneLiveness::Live);
    const DEAD: MockLiveness = MockLiveness(PaneLiveness::Dead);
    const UNKNOWN: MockLiveness = MockLiveness(PaneLiveness::Unknown);

    fn state_owned(pane: &str, uuid: &str) -> Value {
        json!({"team_owner": {"pane_id": pane, "leader_session_uuid": uuid, "provider": "codex"}})
    }

    #[test]
    fn stop_ancestor_authority_requires_persisted_chain_live_owner_and_same_server() {
        let mut state = json!({"teams":{
            "root":{"status":"alive", "team_owner":{"pane_id":"%root","tmux_socket":"/tmp/owner-server"}},
            "child":{"parent_team_key":"root", "team_owner":{"pane_id":"%manager"}},
            "grandchild":{"parent_team_key":"child", "team_owner":{"pane_id":"%researcher"}},
            "sibling":{"team_owner":{"pane_id":"%other","tmux_socket":"/tmp/owner-server"}}
        }});
        let mut root = caller("%root", "root");
        root.tmux_socket = Some("/tmp/owner-server".to_string());
        assert!(!ancestor_owner_allowed(&state, "child", &root, "/tmp/owner-server", "%other", &LIVE));
        assert!(ancestor_owner_allowed(&state, "child", &root, "/tmp/owner-server", "%root", &LIVE));
        assert!(ancestor_owner_allowed(&state, "grandchild", &root, "/tmp/owner-server", "%root", &LIVE));
        assert!(!ancestor_owner_allowed(&state, "sibling", &root, "/tmp/owner-server", "%root", &LIVE));
        assert!(!ancestor_owner_allowed(&state, "child", &caller("%other", "other"), "/tmp/owner-server", "%other", &LIVE));
        assert!(!ancestor_owner_allowed(&state, "child", &root, "/tmp/wrong-server", "%root", &LIVE));
        for probe in [&DEAD, &UNKNOWN] {
            assert!(!ancestor_owner_allowed(&state, "child", &root, "/tmp/owner-server", "%root", probe));
        }
        assert!(check_team_owner(&state["teams"]["child"], &root, false, &LIVE).is_some(),
            "ordinary owner admission must not acquire ancestor authority");
        state["teams"]["root"]["parent_team_key"] = json!("grandchild");
        assert!(team_ancestors(&state, "child").is_err());
        assert!(!ancestor_owner_allowed(&state, "child", &root, "/tmp/owner-server", "%root", &LIVE));
        state["teams"]["root"]["parent_team_key"] = json!("missing");
        assert!(!ancestor_owner_allowed(&state, "child", &root, "/tmp/owner-server", "%root", &LIVE));
    }

    #[test]
    fn recorded_socket_qualifies_every_owner_grant() {
        let mut state = state_owned("%0", "uuid-owner");
        state["team_owner"]["tmux_socket"] = json!("/tmp/owner.sock");
        for pane in ["%0", ""] {
            let mut c = caller(pane, "uuid-owner");
            c.tmux_socket = Some("/tmp/owner.sock".to_string());
            assert!(check_team_owner(&state, &c, false, &LIVE).is_none());
            for socket in [Some("/tmp/foreign.sock".to_string()), None] {
                c.tmux_socket = socket;
                for probe in [&LIVE, &DEAD, &UNKNOWN] {
                    assert!(check_team_owner(&state, &c, false, probe).is_some());
                }
            }
        }
    }

    #[test]
    fn receiver_socket_fallback_preserves_owner_precedence_and_legacy_matching() {
        let mut state = state_owned("%0", "uuid-owner");
        let mut c = caller("%0", "uuid-other");
        assert!(check_team_owner(&state, &c, true, &LIVE).is_none());
        state["leader_receiver"] = json!({"tmux_socket": "/tmp/receiver.sock"});
        assert!(check_team_owner(&state, &c, true, &LIVE).is_some());
        c.tmux_socket = Some("/tmp/receiver.sock".to_string());
        assert!(check_team_owner(&state, &c, true, &LIVE).is_none());
        state["team_owner"]["tmux_socket"] = json!("/tmp/owner.sock");
        assert!(check_team_owner(&state, &c, true, &LIVE).is_some());
        c.tmux_socket = Some("/tmp/owner.sock".to_string());
        assert!(check_team_owner(&state, &c, true, &LIVE).is_none());
    }

    #[test]
    fn no_owner_allows() {
        assert!(check_team_owner(&json!({}), &caller("%c", "u"), true, &LIVE).is_none());
        assert!(
            check_team_owner(&json!({"team_owner": {}}), &caller("%c", "u"), true, &LIVE).is_none()
        );
    }

    #[test]
    fn same_pane_is_own() {
        let s = state_owned("%owner", "uuid-owner");
        assert!(check_team_owner(&s, &caller("%owner", "different-uuid"), true, &LIVE).is_none());
    }

    #[test]
    fn foreign_pane_live_owner_refuses() {
        let s = state_owned("%owner", "uuid-owner");
        let r = check_team_owner(&s, &caller("%intruder", "uuid-intruder"), true, &LIVE).unwrap();
        assert_eq!(r["reason"], json!("team_owner_mismatch"));
        assert_eq!(r["reason_kind"], json!("owner_takeover_required"));
        assert_eq!(r["action"], json!("team-agent claim-leader --confirm"));
        assert_eq!(r["error"], json!("not_owner"));
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn dead_owner_pane_lets_new_live_caller_take_over() {
        // bug:死 owner pane 不该锁住新 live caller(pane-as-identity)。
        let s = state_owned("%owner", "uuid-owner");
        // 非 worker(has_team_agent_id=false)+ 死 owner pane → 允许。
        assert!(check_team_owner(&s, &caller("%new", "uuid-new"), false, &DEAD).is_none());
        // 但 worker(has_team_agent_id=true)即使 owner pane 死也不接管 → 拒绝。
        assert!(check_team_owner(&s, &caller("%new", "uuid-new"), true, &DEAD).is_some());
    }

    #[test]
    fn same_uuid_no_pane_is_own_sticky_bind_when_pane_differs() {
        let s = state_owned("%owner", "uuid-shared");
        // 同 uuid 无 caller_pane → own。
        assert!(check_team_owner(&s, &caller("", "uuid-shared"), true, &LIVE).is_none());
        // 同 uuid 但不同 live pane → sticky_bind_collision 拒绝。
        let r = check_team_owner(&s, &caller("%other", "uuid-shared"), true, &LIVE).unwrap();
        assert_eq!(r["reason_kind"], json!("sticky_bind_collision"));
        assert_eq!(r["action"], json!("team-agent claim-leader --confirm"));
    }


    // §11 血泪:/tmp → /private/tmp 软链不对称(macOS 真机 bug)。两边 canonicalize → match。
    #[test]
    fn realpath_tmp_symlink_asymmetry_matches() {
        // 真实存在的目录(canonicalize 需要路径存在)。
        let dir = std::env::temp_dir().join(format!("ta_rs_og_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // /tmp/x 与 /private/tmp/x:macOS 上 /tmp 是 /private/tmp 的软链。
        let raw = Path::new("/tmp").join(dir.file_name().unwrap());
        let private = Path::new("/private/tmp").join(dir.file_name().unwrap());
        if raw.exists() && private.exists() {
            assert!(
                (realpath_like(&raw) == realpath_like(&private)),
                "/tmp 与 /private/tmp 应 canonicalize 相等"
            );
        }
        // 自反:同路径 match。
        assert!((realpath_like(&dir) == realpath_like(&dir)));
    }

    #[test]
    fn realpath_sibling_prefix_does_not_match() {
        // §11 禁前缀:/repo 与 /repo-backup 共享前缀但是不同目录 → 不 match。
        let base = std::env::temp_dir().join(format!("ta_rs_sib_{}", std::process::id()));
        let repo = base.join("repo");
        let repo_backup = base.join("repo-backup");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&repo_backup).unwrap();
        assert!(
            !(realpath_like(&repo) == realpath_like(&repo_backup)),
            "共享前缀的兄弟目录不得 match"
        );
        // 子目录也不 match(禁子串/反推)。
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(!(realpath_like(&repo) == realpath_like(&sub)));
    }

    // 对抗 P1:拒绝 dict 的 caller 含 5 字段(含 leader_session_uuid_source)。
    #[test]
    fn refusal_caller_has_five_fields() {
        let s = state_owned("%owner", "uuid-owner");
        let r = check_team_owner(&s, &caller("%intruder", "uuid-x"), true, &LIVE).unwrap();
        let c = r["caller"].as_object().unwrap();
        assert_eq!(c.len(), 5);
        assert_eq!(r["caller"]["leader_session_uuid_source"], json!("derived"));
    }

    // 对抗 P0-A(security):owner 无 uuid + caller 无 uuid 不得 "" == "" allow-flip → 必拒绝。
    #[test]
    fn empty_uuids_do_not_allow_flip() {
        // owner 缺 uuid(未迁移形),caller 也缺 uuid,pane 不同 → 必须拒绝(不得误判 own)。
        let s = json!({"team_owner": {"pane_id": "%owner", "provider": "codex"}});
        let mut c = caller("%intruder", "");
        c.leader_session_uuid_source = "derived".to_string();
        let r = check_team_owner(&s, &c, true, &LIVE);
        assert!(r.is_some(), "两边空 uuid 不得 allow;必落拒绝");
        assert_eq!(
            r.unwrap()["reason_kind"],
            json!("owner_takeover_required"),
            "非 sticky_bind(空 uuid 不算同)"
        );
    }

    // 对抗 P2:owner pane liveness == Unknown 时,非 worker caller 也接管;worker 不接管。
    #[test]
    fn unknown_owner_pane_allows_nonworker_takeover() {
        let s = state_owned("%owner", "uuid-owner");
        assert!(
            check_team_owner(&s, &caller("%new", "uuid-new"), false, &UNKNOWN).is_none(),
            "Unknown owner pane + 非 worker → 接管"
        );
        assert!(
            check_team_owner(&s, &caller("%new", "uuid-new"), true, &UNKNOWN).is_some(),
            "worker 即便 Unknown 也不接管"
        );
    }

    // 空 caller_pane + 异 uuid + 死 owner:死-owner 接管分支需 caller_pane 非空,故落拒绝。
    #[test]
    fn empty_caller_pane_diff_uuid_refuses_even_dead_owner() {
        let s = state_owned("%owner", "uuid-owner");
        let r = check_team_owner(&s, &caller("", "uuid-diff"), false, &DEAD);
        assert!(
            r.is_some(),
            "空 caller_pane 不触发死-owner 接管;异 uuid → 拒绝"
        );
    }

    // 对抗 P0-B(§11 血泪):父目录是软链 + 末段不存在 → realpath_like 解析父软链 → 判 own。
    #[test]
    fn parent_symlink_with_missing_leaf_matches() {
        let base = std::env::temp_dir().join(format!("ta_rs_plink_{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(unix)]
        {
            // link/ghost 与 real/ghost:ghost 不存在,但 link 是 real 的软链 → 两边 realpath 相等。
            assert!(
                (realpath_like(&link.join("ghost")) == realpath_like(&real.join("ghost"))),
                "父软链 + 缺失末段应判 own(realpath_like 解析父软链)"
            );
            // 不同末段仍不 match。
            assert!(!(realpath_like(&link.join("ghostA")) == realpath_like(&real.join("ghostB"))));
        }
    }

    // cr 裁决(2026-06-02)双 FS 正确性钉死(见 contracts-rust-native.yaml DECISIONS):
    // 运行时探测当前 FS 大小写敏感性,对该 FS 断言对应的正确行为 —— 同一测试在两种 FS 都自证,
    // 防「保留 canonicalize」这个有意分歧静默漂移。
    #[test]
    fn case_sensitivity_ruling_correct_on_both_fs() {
        let base = std::env::temp_dir().join(format!("ta_rs_case_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let upper = base.join("Workspace");
        std::fs::create_dir_all(&upper).unwrap();
        let lower_variant = base.join("workspace"); // 仅大小写不同

        // 探测:小写变体能否解析到已建的大写目录?能 → case-insensitive FS。
        let fs_case_insensitive = lower_variant.exists();

        if fs_case_insensitive {
            // case-insensitive FS(macOS APFS 默认 / Windows NTFS):同一 inode → 须判 own。
            assert!(
                (realpath_like(&upper) == realpath_like(&lower_variant)),
                "case-insensitive FS:大小写变体是同一目录,canonicalize 须相等 → own"
            );
        } else {
            // case-sensitive FS(Linux ext4 / case-sensitive APFS 卷):真造出第二个目录 → 须判 foreign。
            std::fs::create_dir_all(&lower_variant).unwrap();
            assert!(
                !(realpath_like(&upper) == realpath_like(&lower_variant)),
                "case-sensitive FS:大小写不同是两个目录 → foreign"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
