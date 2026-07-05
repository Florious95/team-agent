//! state.json 持久化(bug-084 韧性;真相源 `state.py:save_runtime_state` + `_self_heal_runtime_state`
//! + `runtime.py:_runtime_lock`)。
//!
//! 流程(逐条对齐 bug_084_085_contract.md):
//! 1. 入参 deep-equal `_RUNTIME_STATE_CACHE[path]` → 取锁/os.replace **之前**返回。
//! 2. `_runtime_lock(workspace, "state-save", timeout=2.0)` 串行化(flock;state-save 不发锁事件)。
//! 3. 原子 `tmp -> rename(tmp, state.json)`;payload = `json.dumps(indent=2, ensure_ascii=False)`(无尾换行)。
//! 4. EACCES/EPERM/EBUSY/PermissionError → 有界退避重试 delays=[0.05,0.2,0.5](4 次尝试);
//!    ENOSPC 等不重试,直接 raise。
//! 5. 重试耗尽且仍 retryable → self-heal:写 heal-tmp → rename(原 state→backup) → rename(heal-tmp→state);
//!    **绝不 in-place truncate**。self-heal 也失败 → 还原 backup(若已建)+ 原 state 仍可见 + raise。
//! 6. 事件:save_retry(每次重试)/ self_healed(成功)/ save_failed(最终失败)/ self_heal_restore_failed。
//! 7. MUST-NOT-13:全程零 provider/network 调用。
//!
//! **已知字节边界(同 event_log,实测 state 不出现;state-rich.json 3471B 字节对拍 PASS)**:
//! `<1e-4` 指数浮点 / `>2^53` 大整数经 serde_json 会漂移;state 字段实测为小整数/字符串/bool/嵌套。
//!
//! **load 迁移(本 slice 接入)**:`load_runtime_state` 现复刻 Python 全链——
//! `normalize_agent_session_state`(SESSION_STATE_FIELDS 补 None)→ `_migrate_state_identity`
//! (`identity::migrate_state_identity`,补 leader_session_uuid)→ `_migrate_active_team_key`
//! (seed active 指针);任一改动 → `save_runtime_state` 回写。不存在且命中缓存 → 返回缓存 deepcopy。

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::event_log::EventLog;
use crate::model::paths::runtime_dir;
use crate::state::identity::{migrate_state_identity, SystemEnv};
use crate::state::json_truthy;
use crate::state::projection::team_state_key;
use crate::state::StateError;

/// `state.py:26-29`:agent session-state 归一字段(setdefault None)。
const SESSION_STATE_FIELDS: [&str; 6] = [
    "session_id",
    "rollout_path",
    "captured_at",
    "captured_via",
    "attribution_confidence",
    "spawn_cwd",
];
const LIVE_TOPOLOGY_FIELDS: [&str; 5] =
    ["pane_id", "pane_pid", "window", "spawned_at", "spawn_epoch"];
const ROSTER_STUB_ALLOWLIST: [&str; 15] = [
    "agent_id",
    "provider",
    "auth_mode",
    "role",
    "model",
    "model_source",
    "profile",
    "_profile_dir",
    "dynamic_role_file",
    "effort",
    "forked_from",
    "managed_mcp_config",
    "claude_config_dir",
    "claude_projects_root",
    "profile_launch",
];

/// `state.py:_RUNTIME_STATE_CACHE`:进程级 path→state 缓存(deep-equal 早返回)。
static RUNTIME_STATE_CACHE: LazyLock<Mutex<HashMap<PathBuf, Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `state.py:41`。
pub fn runtime_state_path(workspace: &Path) -> PathBuf {
    runtime_dir(workspace).join("state.json")
}

fn cache_equals(path: &Path, state: &Value) -> bool {
    RUNTIME_STATE_CACHE
        .lock()
        .is_ok_and(|c| c.get(path) == Some(state))
}
fn cache_set(path: &Path, state: &Value) {
    if let Ok(mut c) = RUNTIME_STATE_CACHE.lock() {
        c.insert(path.to_path_buf(), state.clone());
    }
}
/// `_RUNTIME_STATE_CACHE.get(...)` → `copy.deepcopy(cached)`(clone = deepcopy)。
fn cache_get(path: &Path) -> Option<Value> {
    RUNTIME_STATE_CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(path).cloned())
}

fn unique_tmp(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!("{name}.{}.{seq}.{suffix}", std::process::id()))
}

// 故障注入(测试):**per-call-index 谓词**。FAULT_PLAN[i] = 第 i 次 atomic_replace 的 errno
// (0=成功)。这是对抗检查的 critical 修复:递减计数器无法表达 self-heal restore 分支需要的
// 非连续 succeed/fail 序列(loop 失败、path→backup 成功、heal→path 失败、restore 成功/失败),
// 故崩溃安全不变量「原 state 经 backup 还原」原先根本不可测。
#[cfg(test)]
thread_local! {
    static FAULT_PLAN: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    static FAULT_IDX: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    {
        let idx = FAULT_IDX.with(|c| {
            let i = c.get();
            c.set(i + 1);
            i
        });
        let errno = FAULT_PLAN.with(|p| p.borrow().get(idx).copied().unwrap_or(0));
        if errno != 0 {
            return Err(io::Error::from_raw_os_error(errno));
        }
    }
    std::fs::rename(from, to)
}

/// `_retryable_replace_error`:PermissionError 或 errno ∈ {EACCES, EPERM, EBUSY}。
fn retryable_replace_error(e: &io::Error) -> bool {
    if let Some(errno) = e.raw_os_error() {
        return errno == libc::EACCES || errno == libc::EPERM || errno == libc::EBUSY;
    }
    e.kind() == io::ErrorKind::PermissionDenied
}

fn errno_name(errno: Option<i32>) -> Option<&'static str> {
    match errno {
        Some(e) if e == libc::EACCES => Some("EACCES"),
        Some(e) if e == libc::EPERM => Some("EPERM"),
        Some(e) if e == libc::EBUSY => Some("EBUSY"),
        Some(e) if e == libc::ENOSPC => Some("ENOSPC"),
        _ => None,
    }
}

/// `runtime.py:_runtime_lock` 的 flock 版(RAII;Drop 释放)。state-save 不发锁事件。
///
/// 0.5.x Windows portability Batch 2: migrated to
/// `crate::platform::file_lock::{try_lock_once_nonblocking, unlock}` so
/// the same polling loop + timeout + `StateError::Locked(name)` shape
/// works on both Unix (`flock`) and Windows (`LockFileEx`) — 1:1
/// semantic mapping. The Batch 0 non-Unix `not_yet_implemented`
/// fallback is now removed (CR C-2 fallback burn-down; grep guard
/// `platform_fallback_burndown_batch0.rs` flipped in this batch).
struct RuntimeLock {
    #[allow(dead_code)]
    file: std::fs::File,
}

impl RuntimeLock {
    fn acquire(workspace: &Path, name: &str, timeout: f64) -> Result<Self, StateError> {
        let lock_path = runtime_dir(workspace).join(format!("{name}.lock"));
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        let start = Instant::now();
        loop {
            match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
                Ok(true) => return Ok(Self { file }),
                Ok(false) => {
                    if start.elapsed().as_secs_f64() >= timeout {
                        return Err(StateError::Locked(name.to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    // Byte-preserving: a real I/O error surfaces as a
                    // `StateError::Io` (via `From<io::Error>`), same as
                    // the current inline code would if `file.open()`
                    // failed. The lock-would-block case took the
                    // Ok(false) arm above.
                    return Err(StateError::from(e));
                }
            }
        }
    }
}

impl Drop for RuntimeLock {
    fn drop(&mut self) {
        // Best-effort unlock. OS releases on handle close if this fails.
        let _ = crate::platform::file_lock::unlock(&self.file);
    }
}

/// `save_runtime_state`(bug-084)。`state` 是 state.json 的内存 Value(插入序保留)。
/// 注:Python 在此还调 `_migrate_state_identity`(identity slice 落地后接入;本 slice 不改 state 内容)。
pub fn save_runtime_state(workspace: &Path, state: &Value) -> Result<(), StateError> {
    save_runtime_state_with_merge_options(workspace, state, &[], None, &[], &[])
}

pub(crate) fn save_runtime_state_reapplying_after_conflict<F>(
    workspace: &Path,
    state: &Value,
    reapply: F,
) -> Result<(), StateError>
where
    F: FnOnce(&mut Value),
{
    match save_runtime_state(workspace, state) {
        Ok(()) => Ok(()),
        Err(StateError::SaveConflict(_)) => {
            let mut latest = load_runtime_state(workspace)?;
            reapply(&mut latest);
            save_runtime_state(workspace, &latest)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn save_runtime_state_with_lifecycle_topology_authority(
    workspace: &Path,
    state: &Value,
    agent_ids: &[&str],
) -> Result<(), StateError> {
    save_runtime_state_with_merge_options(workspace, state, &[], None, &[], agent_ids)
}

pub(crate) fn save_runtime_state_with_deleted_agents(
    workspace: &Path,
    state: &Value,
    deleted_agent_ids: &[&str],
) -> Result<(), StateError> {
    save_runtime_state_with_merge_options(workspace, state, deleted_agent_ids, None, &[], &[])
}

pub(crate) fn save_runtime_state_with_team_tombstoned_agents(
    workspace: &Path,
    state: &Value,
    tombstoned_team_key: &str,
    tombstoned_agent_ids: &[&str],
) -> Result<(), StateError> {
    save_runtime_state_with_merge_options(
        workspace,
        state,
        &[],
        Some(tombstoned_team_key),
        tombstoned_agent_ids,
        &[],
    )
}

pub(crate) fn save_runtime_state_with_team_tombstone_lifecycle_topology_authority(
    workspace: &Path,
    state: &Value,
    tombstoned_team_key: &str,
    agent_ids: &[&str],
) -> Result<(), StateError> {
    save_runtime_state_with_merge_options(
        workspace,
        state,
        &[],
        Some(tombstoned_team_key),
        agent_ids,
        agent_ids,
    )
}

fn save_runtime_state_with_merge_options(
    workspace: &Path,
    state: &Value,
    deleted_agent_ids: &[&str],
    skip_capture_backfill_team_key: Option<&str>,
    skip_capture_backfill_agent_ids: &[&str],
    topology_update_agent_ids: &[&str],
) -> Result<(), StateError> {
    let path = runtime_state_path(workspace);
    // Python `state.py:497`:先对入参 state 跑 `_migrate_state_identity`(就地填缺失 leader uuid)。
    // 我们 `&Value` 不可变 → 克隆后迁移,后续比较/写入/缓存/self-heal 全走 `migrated`。
    // 该步**不**包 try/except → 错误 propagate(对齐 Python)。
    let mut migrated = state.clone();
    migrate_state_identity(&mut migrated, &SystemEnv, workspace)?;
    // Stage 3 save-output canonical-aware strip (architect direction
    // 2026-06-24, .team/artifacts/stage3-save-strip-fix.md): when the
    // state carries a canonical `teams.<active>.team_owner` record, drop
    // the legacy top-level `team_owner / leader_receiver / owner_epoch`
    // before any cache comparison. The pre-fix order ran a `cache_equals`
    // check against the RAW (pre-strip) state at the function entry — if
    // disk and cache were already in the dual-source (post-restart /
    // post-shutdown) shape, the early return would skip the cleanup
    // entirely. Moving migrate+strip ahead of every cache_equals makes
    // the canonical-only shape the cache invariant for new writes.
    crate::state::ownership::strip_top_level_ownership_if_canonical_present(&mut migrated);
    if cache_equals(&path, &migrated) {
        return Ok(());
    }
    // 与磁盘已有内容「迁移后」相同 → 更新缓存返回(避免无谓重写)。字节对拍 Python:对 `existing` 先
    // `normalize_agent_session_state` + `_migrate_state_identity` 再比(读/迁移失败 try/except: pass →
    // 落写路径)。**修对抗 P1**:此前比较 raw `existing` 漏了这两步,会把「磁盘已是迁移等价形」的 legacy
    // 文件误判为不同而 spurious 重写,破坏 load+save 字节恒等。
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut existing) = serde_json::from_str::<Value>(&text) {
                normalize_agent_session_state(&mut existing);
                let _ = migrate_state_identity(&mut existing, &SystemEnv, workspace);
                // Stage 3 save-output strip applied to `existing` too so
                // the equality comparison is between two canonical-only
                // shapes. Without this, a pre-fix dual-source disk state
                // would never match the post-fix canonical-only `migrated`
                // and would force a spurious rewrite on every save.
                crate::state::ownership::strip_top_level_ownership_if_canonical_present(
                    &mut existing,
                );
                if existing == migrated {
                    cache_set(&path, &migrated);
                    return Ok(());
                }
            }
        }
    }

    let _lock = RuntimeLock::acquire(workspace, "state-save", 2.0)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Some(latest) = read_latest_state_under_lock(workspace, &path) {
        let deleted = deleted_agent_ids
            .iter()
            .copied()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let skip_capture_backfill = skip_capture_backfill_agent_ids
            .iter()
            .copied()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let topology_updates = topology_update_agent_ids
            .iter()
            .copied()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        apply_persist_merge_contract(
            &mut migrated,
            &latest,
            &deleted,
            skip_capture_backfill_team_key,
            &skip_capture_backfill,
            &topology_updates,
        )?;
    }
    // Stage 3 save-output strip second pass (defence-in-depth): after the
    // `preserve_latest_roster_entries` lock-held merge, a future addition
    // to the preserve path could re-introduce root owner fields from the
    // on-disk latest. Re-strip here so the serialized payload is always
    // canonical-only when a canonical teams.<key>.team_owner is present.
    crate::state::ownership::strip_top_level_ownership_if_canonical_present(&mut migrated);
    // 字节对拍 Python json.dumps(indent=2, ensure_ascii=False)(无尾换行)。
    let payload = serde_json::to_string_pretty(&migrated)?;
    let delays = [0.05_f64, 0.2, 0.5];

    for attempt in 0..=delays.len() {
        let tmp = unique_tmp(&path, "tmp");
        let result: io::Result<()> = (|| {
            std::fs::write(&tmp, payload.as_bytes())?;
            atomic_replace(&tmp, &path)
        })();
        let _ = std::fs::remove_file(&tmp); // finally: unlink missing_ok
        match result {
            Ok(()) => {
                cache_set(&path, &migrated);
                return Ok(());
            }
            Err(e) => {
                let retryable = retryable_replace_error(&e);
                if !retryable || attempt >= delays.len() {
                    if retryable {
                        return self_heal(workspace, &path, &payload, &migrated, attempt + 1, &e);
                    }
                    return Err(StateError::Io(e));
                }
                let _ = EventLog::new(workspace).write(
                    "runtime.state.save_retry",
                    json!({
                        "attempt": attempt + 1,
                        "errno": e.raw_os_error(),
                        "errno_name": errno_name(e.raw_os_error()),
                        "error": e.to_string(),
                    }),
                );
                std::thread::sleep(Duration::from_secs_f64(delays[attempt]));
            }
        }
    }
    Err(StateError::SaveFailed(
        "retry loop exhausted without return".to_string(),
    ))
}

fn read_latest_state_under_lock(workspace: &Path, path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut latest = serde_json::from_str::<Value>(&text).ok()?;
    normalize_agent_session_state(&mut latest);
    let _ = migrate_state_identity(&mut latest, &SystemEnv, workspace);
    let _ = migrate_active_team_key(&mut latest);
    Some(latest)
}

fn apply_persist_merge_contract(
    incoming: &mut Value,
    latest: &Value,
    deleted_agent_ids: &BTreeSet<String>,
    skip_capture_backfill_team_key: Option<&str>,
    skip_capture_backfill_agent_ids: &BTreeSet<String>,
    topology_update_agent_ids: &BTreeSet<String>,
) -> Result<(), StateError> {
    // A0/R1: the projection gate only guards the TOP-LEVEL passes (top-level agents and
    // the top-level<->active-team cross projections depend on which team is active); the
    // per-team `teams.<k>.agents` merge below is team-key self-identifying and must run
    // even when another process flipped session_name/active_team_key between this
    // writer's load and save.
    let projection_matches = same_runtime_projection(incoming, latest);
    let top_level_team = Some(team_state_key(incoming)).or_else(|| Some(team_state_key(latest)));
    let skip_top_level_capture_backfill =
        should_skip_capture_backfill(top_level_team.as_deref(), skip_capture_backfill_team_key);
    if projection_matches {
        merge_agent_projection(
            "agents",
            incoming.get_mut("agents"),
            latest.get("agents"),
            deleted_agent_ids,
            skip_top_level_capture_backfill,
            skip_capture_backfill_agent_ids,
            topology_update_agent_ids,
        )?;
        // Stage 3c (identity-boundary unified plan, architect direction
        // 2026-06-23): top-level owner copy-back removed. Pre-3c this
        // copied legacy `state.{team_owner, leader_receiver, owner_epoch}`
        // from disk's latest back into the incoming save — the persist-side
        // mirror of the projection promote that 3b removed. With Stage 3a's
        // write_owner funneling all owner mutations and 3b's projection
        // cleanup, top-level owner truth is no longer authoritative; the
        // teams.<key> entry-scoped preservation at :370 remains so legacy
        // teams.<key>.team_owner entries still survive a save that omits
        // them.
    }

    let latest_teams = latest.get("teams").and_then(Value::as_object);
    let Some(incoming_teams) = incoming.get_mut("teams").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    if let Some(latest_teams) = latest_teams {
        for (team, latest_entry) in latest_teams {
            let Some(incoming_entry) = incoming_teams.get_mut(team) else {
                continue;
            };
            let projection = format!("teams.{team}.agents");
            merge_agent_projection(
                &projection,
                incoming_entry.get_mut("agents"),
                latest_entry.get("agents"),
                deleted_agent_ids,
                should_skip_capture_backfill(Some(team), skip_capture_backfill_team_key),
                skip_capture_backfill_agent_ids,
                topology_update_agent_ids,
            )?;
            preserve_latest_ownership_fields(incoming_entry, latest_entry);
        }
    }
    Ok(())
}

fn should_skip_capture_backfill(
    current_team_key: Option<&str>,
    skip_team_key: Option<&str>,
) -> bool {
    match skip_team_key {
        Some(skip_team_key) => current_team_key == Some(skip_team_key),
        None => true,
    }
}

fn preserve_latest_ownership_fields(incoming: &mut Value, latest: &Value) {
    if !latest_has_preferable_ownership(incoming, latest) {
        return;
    }
    let Some(incoming_obj) = incoming.as_object_mut() else {
        return;
    };
    for key in ["leader_receiver", "team_owner", "owner_epoch"] {
        if let Some(value) = latest.get(key).filter(|value| json_truthy(value)) {
            incoming_obj.insert(key.to_string(), value.clone());
        }
    }
}

fn latest_has_preferable_ownership(incoming: &Value, latest: &Value) -> bool {
    let latest_epoch = ownership_epoch(latest);
    let incoming_epoch = ownership_epoch(incoming);
    if latest_epoch > incoming_epoch {
        return true;
    }
    latest_epoch == incoming_epoch && !ownership_attached(incoming) && ownership_attached(latest)
}

fn ownership_epoch(state: &Value) -> u64 {
    state
        .get("owner_epoch")
        .and_then(Value::as_u64)
        .or_else(|| {
            state
                .get("team_owner")
                .and_then(|owner| owner.get("owner_epoch"))
                .and_then(Value::as_u64)
        })
        .or_else(|| {
            state
                .get("leader_receiver")
                .and_then(|receiver| receiver.get("owner_epoch"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0)
}

fn ownership_attached(state: &Value) -> bool {
    ["leader_receiver", "team_owner"].into_iter().any(|key| {
        state
            .get(key)
            .and_then(|value| value.get("pane_id"))
            .and_then(Value::as_str)
            .is_some_and(|pane| !pane.is_empty() && pane != "__team_agent_unbound__")
    })
}

fn merge_agent_projection(
    projection: &str,
    incoming_agents: Option<&mut Value>,
    latest_agents: Option<&Value>,
    deleted_agent_ids: &BTreeSet<String>,
    skip_capture_backfill: bool,
    skip_capture_backfill_agent_ids: &BTreeSet<String>,
    topology_update_agent_ids: &BTreeSet<String>,
) -> Result<(), StateError> {
    let Some(incoming_agents) = incoming_agents else {
        return Ok(());
    };
    let Some(incoming_map) = incoming_agents.as_object_mut() else {
        return Ok(());
    };
    let Some(latest_map) = latest_agents.and_then(Value::as_object) else {
        return Ok(());
    };
    for (agent_id, latest_agent) in latest_map {
        if deleted_agent_ids.contains(agent_id) {
            continue;
        }
        let tombstoned_for_projection =
            skip_capture_backfill && skip_capture_backfill_agent_ids.contains(agent_id);
        match incoming_map.entry(agent_id.clone()) {
            serde_json::map::Entry::Vacant(slot) => {
                if topology_update_agent_ids.contains(agent_id) {
                    continue;
                }
                if !tombstoned_for_projection && latest_has_live_topology(latest_agent) {
                    return Err(save_conflict(
                        projection,
                        agent_id,
                        live_topology_fields(latest_agent),
                    ));
                }
                if let Some(stub) = roster_stub(latest_agent) {
                    slot.insert(stub);
                }
            }
            serde_json::map::Entry::Occupied(mut existing) => {
                if !tombstoned_for_projection && !topology_update_agent_ids.contains(agent_id) {
                    let fields = topology_conflict_fields(existing.get(), latest_agent);
                    if !fields.is_empty() {
                        return Err(save_conflict(projection, agent_id, fields));
                    }
                }
                if !skip_capture_backfill || !skip_capture_backfill_agent_ids.contains(agent_id) {
                    backfill_capture_fields(existing.get_mut(), latest_agent);
                }
            }
        }
    }
    Ok(())
}

fn save_conflict(projection: &str, agent_id: &str, fields: Vec<&'static str>) -> StateError {
    StateError::SaveConflict(format!(
        "agent_id={agent_id} projection={projection} conflicting_fields={}",
        fields.join(",")
    ))
}

fn latest_has_live_topology(agent: &Value) -> bool {
    LIVE_TOPOLOGY_FIELDS
        .iter()
        .any(|field| agent.get(field).is_some_and(json_truthy))
}

fn live_topology_fields(agent: &Value) -> Vec<&'static str> {
    LIVE_TOPOLOGY_FIELDS
        .iter()
        .copied()
        .filter(|field| agent.get(*field).is_some_and(json_truthy))
        .collect()
}

fn topology_conflict_fields(incoming_agent: &Value, latest_agent: &Value) -> Vec<&'static str> {
    let latest_live = live_topology_fields(latest_agent);
    if latest_live.is_empty() {
        return Vec::new();
    }
    latest_live
        .into_iter()
        .filter(|field| incoming_agent.get(*field) != latest_agent.get(*field))
        .collect()
}

fn roster_stub(latest_agent: &Value) -> Option<Value> {
    let latest = latest_agent.as_object()?;
    let mut stub = serde_json::Map::new();
    for field in ROSTER_STUB_ALLOWLIST {
        if let Some(value) = latest.get(field).filter(|value| !value.is_null()) {
            stub.insert(field.to_string(), value.clone());
        }
    }
    (!stub.is_empty()).then(|| Value::Object(stub))
}

/// 0.4.6 tuple-atomic backfill (restart-persist-capture-contract-audit.md):
/// the authoritative session tuple is `session_id + rollout_path +
/// captured_at + captured_via`. `attribution_confidence` is metadata on
/// that tuple. Persist may preserve an already-complete tuple across stale
/// saves, but it must NEVER:
///   * synthesise a partial tuple (e.g. copy `session_id` while the other
///     three are absent — this caused bug-045 by resurrecting an
///     intentionally cleared session id);
///   * mix tuple fields across two different session_ids (one writer's
///     `session_id` glued to another writer's `rollout_path`);
///   * create non-null session truth where none existed.
///
/// Rules:
///   1. If LATEST has a complete tuple AND INCOMING has the same `session_id`
///      (or null), backfill the full tuple together — concurrent capture
///      protection (A0/R2).
///   2. If LATEST has a complete tuple AND INCOMING has a DIFFERENT non-null
///      `session_id`, copy nothing (incoming is a different worker session
///      identity; the latest tuple belongs to the previous identity).
///   3. If LATEST tuple is INCOMPLETE (any of the 4 fields null), copy
///      nothing for the tuple. An incomplete latest tuple is not
///      authoritative truth, so persist cannot use it as backfill source.
///   4. `attribution_confidence` rides with the tuple (copied iff tuple
///      backfill fires).
fn backfill_capture_fields(incoming_agent: &mut Value, latest_agent: &Value) {
    const TUPLE_FIELDS: [&str; 4] = ["session_id", "rollout_path", "captured_at", "captured_via"];
    let Some(incoming_row) = incoming_agent.as_object_mut() else {
        return;
    };
    // Rule 3: latest tuple must be COMPLETE to be a backfill source.
    let latest_complete = TUPLE_FIELDS
        .iter()
        .all(|field| latest_agent.get(field).is_some_and(|v| !v.is_null()));
    if !latest_complete {
        return;
    }
    // S1-CAPTURE-001 (0.4.8) Rule 5: a fresh restart marker (`_pending_session_id`
    // present on incoming) signals the worker just respawned and the prior
    // tuple was intentionally cleared. The capture scanner is the only path
    // permitted to promote `_pending_session_id` into the authoritative tuple
    // after it confirms backing. Persist-side backfill MUST NOT revive the
    // latest tuple in this state — doing so resurrects the previous worker's
    // session/rollout pair and delivered tokens land in the OLD transcript
    // (the leader/unassigned mis-attribution surfaced in S1-CAPTURE-001 gate
    // evidence). Refuse backfill regardless of whether incoming session_id
    // is null vs latest's value: the pending marker is authoritative intent.
    let incoming_pending = incoming_row
        .get("_pending_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if incoming_pending.is_some() {
        return;
    }
    // Rule 2: incoming carries a DIFFERENT non-null session_id → do not mix.
    let incoming_session = incoming_row.get("session_id").and_then(Value::as_str);
    let latest_session = latest_agent.get("session_id").and_then(Value::as_str);
    if let Some(inc) = incoming_session {
        if Some(inc) != latest_session {
            return;
        }
    }
    // Rule 1 + 4: copy the full tuple together (only fields incoming has as
    // null/missing) plus attribution_confidence.
    for field in TUPLE_FIELDS
        .iter()
        .copied()
        .chain(std::iter::once("attribution_confidence"))
    {
        if incoming_row.get(field).is_none_or(Value::is_null) {
            if let Some(value) = latest_agent.get(field).filter(|value| !value.is_null()) {
                incoming_row.insert(field.to_string(), value.clone());
            }
        }
    }
}

fn same_runtime_projection(left: &Value, right: &Value) -> bool {
    let left_session = left.get("session_name").and_then(Value::as_str);
    let right_session = right.get("session_name").and_then(Value::as_str);
    if left_session.is_some() && right_session.is_some() && left_session != right_session {
        return false;
    }
    let left_team = active_team_key(left);
    let right_team = active_team_key(right);
    if left_team.is_some() && right_team.is_some() && left_team != right_team {
        return false;
    }
    true
}

fn active_team_key(state: &Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty() && *team != "current")
        .map(str::to_string)
}

/// `_self_heal_runtime_state`:重建 inode(heal-tmp + backup-rename),绝不 in-place truncate。
fn self_heal(
    workspace: &Path,
    path: &Path,
    payload: &str,
    state: &Value,
    attempts_used: usize,
    original_exc: &io::Error,
) -> Result<(), StateError> {
    let event_log = EventLog::new(workspace);
    let heal_tmp = unique_tmp(path, "heal.tmp");
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let backup = path.with_file_name(format!("{name}.bak.{}", std::process::id()));
    let mut backup_created = false;

    let outcome: io::Result<()> = (|| {
        std::fs::write(&heal_tmp, payload.as_bytes())?;
        match atomic_replace(path, &backup) {
            Ok(()) => backup_created = true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {} // 原 state 不存在 → 无需备份
            Err(e) => return Err(e),
        }
        atomic_replace(&heal_tmp, path)
    })();
    let _ = std::fs::remove_file(&heal_tmp); // finally

    match outcome {
        Ok(()) => {
            cache_set(path, state);
            let _ = event_log.write(
                "runtime.state.self_healed",
                json!({
                    "inode_rebuilt": true,
                    "attempts_used": attempts_used,
                    "replace_retries": attempts_used.saturating_sub(1),
                }),
            );
            Ok(())
        }
        Err(e) => {
            if backup_created {
                if let Err(restore) = atomic_replace(&backup, path) {
                    let _ = event_log.write(
                        "runtime.state.self_heal_restore_failed",
                        json!({"error": restore.to_string()}),
                    );
                }
            }
            let _ = event_log.write(
                "runtime.state.save_failed",
                json!({
                    "phase": "save_runtime_state",
                    "final_errno": e.raw_os_error().or_else(|| original_exc.raw_os_error()),
                    "error": e.to_string(),
                    "retries_used": attempts_used.saturating_sub(1),
                }),
            );
            Err(StateError::SaveFailed(e.to_string()))
        }
    }
}

/// `load_runtime_state`(本 slice 最小:读+parse+缓存;normalize/migration 待 identity slice)。
/// `normalize_agent_session_state`(`state.py:45`):为每个 agent dict 的 SESSION_STATE_FIELDS
/// setdefault None(缺则末尾插)。
pub fn normalize_agent_session_state(state: &mut Value) {
    let Some(agents) = state.get_mut("agents").and_then(Value::as_object_mut) else {
        return;
    };
    for agent_state in agents.values_mut() {
        if let Some(obj) = agent_state.as_object_mut() {
            for field in SESSION_STATE_FIELDS {
                obj.entry(field.to_string()).or_insert(Value::Null);
            }
        }
    }
}

/// `_migrate_active_team_key`(`state.py:73`,0.2.6 Family B C6):legacy state 缺 active_team_key
/// 时 seed 一次。返回是否有改动。
///
/// 注:Python `seed if seed in teams or not teams else seed` 两支均为 `seed`(死三元),
/// 此处直接 `= seed`,与可观测行为一致。
pub fn migrate_active_team_key(state: &mut Value) -> bool {
    if state
        .as_object()
        .is_some_and(|o| o.contains_key("active_team_key"))
    {
        return false;
    }
    let teams_is_dict = state.get("teams").is_some_and(Value::is_object);
    let teams_len = state
        .get("teams")
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    if state.get("session_name").is_some_and(json_truthy) {
        let seed = team_state_key(state);
        if let Some(o) = state.as_object_mut() {
            o.insert("active_team_key".to_string(), Value::String(seed));
        }
        return true;
    }
    if teams_is_dict && teams_len == 1 {
        let first = state
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|t| t.keys().next().cloned());
        if let (Some(k), Some(o)) = (first, state.as_object_mut()) {
            o.insert("active_team_key".to_string(), Value::String(k));
        }
        return true;
    }
    if let Some(o) = state.as_object_mut() {
        o.insert("active_team_key".to_string(), Value::Null);
    }
    true
}

/// RM-039-STAT-001 second-round compat normalizer (architect verdict
/// 2026-06-22). When `active_team_key` names an existing `teams` entry
/// but root `team_key` is missing, set root `team_key = active_team_key`
/// (and mirror it into `teams[active_team_key].team_key` for callers
/// that read the team-scoped slot). This keeps the cascade in
/// `state::projection::team_state_key` honest: the first branch
/// (`state.team_key`) now matches what `active_team_key` says, so
/// coordinator tick's `save_team_scoped_state` writes activity to the
/// SAME teams entry the status selector later reads.
///
/// Narrow on purpose:
///   * Returns `false` (no-op) when root `team_key` already exists,
///     even if it disagrees with `active_team_key`. Conflict cases
///     stay observable; we don't silently rewrite.
///   * Returns `false` when `active_team_key` is null/empty.
///   * Returns `false` when `teams[active_team_key]` does not exist.
///   * Does NOT touch any other field (no generic deep merge).
///
/// Returns `true` if state was mutated and the caller should persist.
pub fn migrate_team_key_to_match_active_team(state: &mut Value) -> bool {
    let Some(obj) = state.as_object() else {
        return false;
    };
    let already_has_root = obj
        .get("team_key")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if already_has_root {
        return false;
    }
    let active = match obj.get("active_team_key").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return false,
    };
    let teams_has_active = obj
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| teams.contains_key(&active));
    if !teams_has_active {
        return false;
    }
    let Some(obj_mut) = state.as_object_mut() else {
        return false;
    };
    obj_mut.insert("team_key".to_string(), Value::String(active.clone()));
    // Mirror inside teams[active] so any code reading the team-scoped
    // slot sees the same canonical key. Skip if the entry isn't an
    // object (defensive — should always be).
    if let Some(team_entry) = obj_mut
        .get_mut("teams")
        .and_then(Value::as_object_mut)
        .and_then(|teams| teams.get_mut(&active))
        .and_then(Value::as_object_mut)
    {
        let needs_team_key = team_entry
            .get("team_key")
            .and_then(Value::as_str)
            .map(|s| s != active)
            .unwrap_or(true);
        if needs_team_key {
            team_entry.insert("team_key".to_string(), Value::String(active));
        }
    }
    true
}

pub fn load_runtime_state(workspace: &Path) -> Result<Value, StateError> {
    let path = runtime_state_path(workspace);
    if !path.exists() {
        if let Some(cached) = cache_get(&path) {
            return Ok(cached);
        }
        return Ok(
            json!({"agents": {}, "tasks": [], "session_name": null, "active_team_key": null}),
        );
    }
    let text = std::fs::read_to_string(&path)?;
    let mut state: Value = serde_json::from_str(&text)?;
    normalize_agent_session_state(&mut state);
    let mut changed = migrate_state_identity(&mut state, &SystemEnv, workspace)?;
    if migrate_active_team_key(&mut state) {
        changed = true;
    }
    // RM-039-STAT-001 second-round compat (architect verdict 2026-06-22):
    // for states written before the launch-side `team_key` fix landed,
    // promote `active_team_key` into root `team_key` so coordinator tick
    // and status selector agree on which `teams` entry to read/write.
    if migrate_team_key_to_match_active_team(&mut state) {
        changed = true;
    }
    if changed {
        save_runtime_state(workspace, &state)?;
    }
    cache_set(&path, &state);
    Ok(state)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use std::sync::atomic::AtomicU32;

    static SEQ: AtomicU32 = AtomicU32::new(0);
    fn temp_ws() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!("ta_rs_state_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }
    fn read_events(ws: &Path) -> Vec<Value> {
        EventLog::new(ws).tail(50).unwrap()
    }
    fn count_event(ws: &Path, name: &str) -> usize {
        read_events(ws)
            .iter()
            .filter(|e| e["event"] == json!(name))
            .count()
    }
    // per-call-index 故障计划:plan[i] = 第 i 次 atomic_replace 的 errno(0=成功)。
    fn set_fault_plan(plan: &[i32]) {
        FAULT_PLAN.with(|p| *p.borrow_mut() = plan.to_vec());
        FAULT_IDX.with(|c| c.set(0));
    }
    fn clear_fault() {
        set_fault_plan(&[]);
    }
    fn get_event(ws: &Path, name: &str) -> Value {
        read_events(ws)
            .into_iter()
            .find(|e| e["event"] == json!(name))
            .unwrap_or(Value::Null)
    }
    fn read_state(ws: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(runtime_state_path(ws)).unwrap()).unwrap()
    }
    fn write_state(ws: &Path, state: &Value) {
        std::fs::create_dir_all(runtime_dir(ws)).unwrap();
        std::fs::write(
            runtime_state_path(ws),
            serde_json::to_string_pretty(state).unwrap(),
        )
        .unwrap();
    }
    fn bak_files(ws: &Path) -> Vec<PathBuf> {
        let dir = runtime_dir(ws);
        std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().contains(".bak."))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // 字节对拍:state-rich.json 经 to_string_pretty 字节 == Python json.dumps(indent=2, ensure_ascii=False)。
    #[test]
    fn state_json_byte_parity_with_python_indent2() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../snapshot/fixtures/bug_084_state_resilience/state-rich.json"
        ));
        let canonical = include_str!("testdata/state-rich.canonical.json");
        let v: Value = serde_json::from_str(fixture).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&v).unwrap(),
            canonical,
            "state.json 序列化未字节对齐 Python indent=2"
        );
    }

    #[test]
    fn save_writes_atomically_and_caches() {
        let ws = temp_ws();
        let state =
            json!({"session_name":"t","agents":{"a":{"agent_id":"a"}},"active_team_key":"t"});
        save_runtime_state(&ws, &state).unwrap();
        let on_disk = std::fs::read_to_string(runtime_state_path(&ws)).unwrap();
        assert_eq!(on_disk, serde_json::to_string_pretty(&state).unwrap());
        assert!(!on_disk.ends_with('\n'), "无尾换行(对齐 json.dumps)");
        // deep-equal 缓存早返回:再 save 相同 state 不应改文件 mtime 行为(此处验返回 Ok 且无错)。
        save_runtime_state(&ws, &state).unwrap();
    }

    #[test]
    fn deep_equal_cache_early_returns() {
        let ws = temp_ws();
        let state = json!({"x":1});
        save_runtime_state(&ws, &state).unwrap();
        // 删掉文件;若缓存早返回生效,save 相同 state 不会重建文件。
        std::fs::remove_file(runtime_state_path(&ws)).unwrap();
        save_runtime_state(&ws, &state).unwrap();
        assert!(
            !runtime_state_path(&ws).exists(),
            "deep-equal 命中缓存 → 未重写(文件仍不存在)"
        );
    }

    // bug-084 核心:EACCES 重试 3 次(有界退避)→ self-heal 成功 + 事件**字段精确**。
    #[test]
    fn retryable_eacces_retries_then_self_heals() {
        let ws = temp_ws();
        save_runtime_state(&ws, &json!({"v":1})).unwrap(); // 原 state(供 self-heal backup-rename)
        let s2 = json!({"v":2});
        set_fault_plan(&[libc::EACCES, libc::EACCES, libc::EACCES, libc::EACCES]); // loop 4 失败,heal 的 2 次 replace 成功
        save_runtime_state(&ws, &s2).unwrap();
        clear_fault();
        assert_eq!(read_state(&ws), s2, "inode 重建,文件为 s2");
        // 事件序列 + 字段精确。
        let retries: Vec<_> = read_events(&ws)
            .into_iter()
            .filter(|e| e["event"] == json!("runtime.state.save_retry"))
            .collect();
        assert_eq!(retries.len(), 3, "3 次重试");
        assert_eq!(retries[0]["attempt"], json!(1));
        assert_eq!(retries[0]["errno_name"], json!("EACCES"));
        assert_eq!(retries[2]["attempt"], json!(3));
        let healed = get_event(&ws, "runtime.state.self_healed");
        assert_eq!(healed["inode_rebuilt"], json!(true));
        assert_eq!(healed["attempts_used"], json!(4));
        assert_eq!(healed["replace_retries"], json!(3));
    }

    // EPERM / EBUSY 也是 retryable(不只 EACCES)。
    #[test]
    fn eperm_and_ebusy_are_retryable() {
        for (errno, name) in [(libc::EPERM, "EPERM"), (libc::EBUSY, "EBUSY")] {
            let ws = temp_ws();
            save_runtime_state(&ws, &json!({"v":1})).unwrap();
            set_fault_plan(&[errno, errno, errno, errno]);
            save_runtime_state(&ws, &json!({"v":2})).unwrap();
            clear_fault();
            assert_eq!(read_state(&ws), json!({"v":2}));
            assert_eq!(count_event(&ws, "runtime.state.self_healed"), 1);
            assert_eq!(
                get_event(&ws, "runtime.state.save_retry")["errno_name"],
                json!(name)
            );
        }
    }

    // 非 retryable(ENOSPC)→ 不重试、不 self-heal,直接 Err。
    #[test]
    fn non_retryable_enospc_raises_without_self_heal() {
        let ws = temp_ws();
        set_fault_plan(&[libc::ENOSPC]);
        let r = save_runtime_state(&ws, &json!({"v":9}));
        clear_fault();
        assert!(matches!(r, Err(StateError::Io(_))), "ENOSPC 直接 raise");
        assert_eq!(count_event(&ws, "runtime.state.self_healed"), 0);
        assert_eq!(count_event(&ws, "runtime.state.save_retry"), 0);
    }

    // 重试边界:恰 3 次重试,第 4 次(attempt 3)成功 → 无 self-heal。钉死 0..=delays.len() 的 off-by-one。
    #[test]
    fn retry_boundary_exactly_three_then_succeeds() {
        let ws = temp_ws();
        set_fault_plan(&[libc::EACCES, libc::EACCES, libc::EACCES]); // 前 3 失败,第 4 次成功
        save_runtime_state(&ws, &json!({"v":7})).unwrap();
        clear_fault();
        assert_eq!(read_state(&ws), json!({"v":7}));
        assert_eq!(count_event(&ws, "runtime.state.save_retry"), 3);
        assert_eq!(
            count_event(&ws, "runtime.state.self_healed"),
            0,
            "未触发 self-heal"
        );
    }

    // 崩溃安全不变量①:self-heal 中途失败但 restore 成功 → 原 state 复位 + 0 restore_failed + 1 save_failed。
    // (per-index 注入器解锁:loop4 失败、path→backup 成功、heal→path 失败、restore 成功。)
    #[test]
    fn self_heal_restore_success_recovers_original() {
        let ws = temp_ws();
        let original = json!({"keep":"original"});
        save_runtime_state(&ws, &original).unwrap();
        let e = libc::EACCES;
        set_fault_plan(&[e, e, e, e, 0, e, 0]); // idx4=path→backup ok, idx5=heal→path fail, idx6=restore ok
        let r = save_runtime_state(&ws, &json!({"keep":"new"}));
        clear_fault();
        assert!(matches!(r, Err(StateError::SaveFailed(_))));
        assert_eq!(
            read_state(&ws),
            original,
            "restore 成功:原 state 复位到 state.json"
        );
        assert_eq!(
            count_event(&ws, "runtime.state.self_heal_restore_failed"),
            0
        );
        let failed = get_event(&ws, "runtime.state.save_failed");
        assert_eq!(failed["phase"], json!("save_runtime_state"));
        assert_eq!(failed["retries_used"], json!(3));
    }

    // 崩溃安全不变量②:self-heal 失败且 restore 也失败 → 原 state 在 .bak 里完好可恢复 + restore_failed 事件。
    #[test]
    fn self_heal_restore_failed_leaves_original_in_backup() {
        let ws = temp_ws();
        let original = json!({"keep":"original"});
        save_runtime_state(&ws, &original).unwrap();
        let e = libc::EACCES;
        set_fault_plan(&[e, e, e, e, 0, e, e]); // idx4=backup ok, idx5=heal fail, idx6=restore fail
        let r = save_runtime_state(&ws, &json!({"keep":"new"}));
        clear_fault();
        assert!(matches!(r, Err(StateError::SaveFailed(_))));
        assert_eq!(
            count_event(&ws, "runtime.state.self_heal_restore_failed"),
            1
        );
        assert_eq!(count_event(&ws, "runtime.state.save_failed"), 1);
        // state.json 已被 rename 到 backup(restore 失败),原 state 在 .bak 里完好(绝不丢失)。
        let baks = bak_files(&ws);
        assert_eq!(baks.len(), 1, "应有一个 .bak 存原 state");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&baks[0]).unwrap()).unwrap();
        assert_eq!(v, original, "原 state 经 backup 可恢复");
    }

    // backup FileNotFound 分支:无前置 state.json → path→backup 当 NotFound 吞,heal 仍成功,无 .bak。
    #[test]
    fn self_heal_with_no_prior_state_swallows_backup_notfound() {
        let ws = temp_ws();
        let e = libc::EACCES;
        set_fault_plan(&[e, e, e, e]); // 仅 loop 失败;heal 的 path→backup(真 NotFound,未注入)+ heal→path 成功
        save_runtime_state(&ws, &json!({"fresh":true})).unwrap();
        clear_fault();
        assert_eq!(read_state(&ws), json!({"fresh":true}));
        assert_eq!(count_event(&ws, "runtime.state.self_healed"), 1);
        assert!(bak_files(&ws).is_empty(), "原 state 不存在 → 不应产生 .bak");
    }

    // non-ASCII 字节透传(ensure_ascii=False):中文/emoji 字面写入,不转 \\uXXXX。
    #[test]
    fn non_ascii_values_pass_through_literally() {
        let ws = temp_ws();
        save_runtime_state(&ws, &json!({"objective":"héllo🦀 世界","x":1})).unwrap();
        let bytes = std::fs::read_to_string(runtime_state_path(&ws)).unwrap();
        assert!(bytes.contains("héllo🦀 世界"), "非 ASCII 应字面保留");
        assert!(!bytes.contains("\\u"), "不应 ascii-escape");
    }

    // deep-equal 命中必须在**取锁之前**返回:持锁时对相同 state 再 save 应立即 Ok(不撞锁 timeout)。
    #[test]
    fn deep_equal_save_returns_before_lock() {
        let ws = temp_ws();
        let state = json!({"v":1});
        save_runtime_state(&ws, &state).unwrap(); // 填充缓存
        let _held = RuntimeLock::acquire(&ws, "state-save", 2.0).unwrap(); // 占锁
                                                                           // 若 deep-equal 不早返回,会去抢已被占的锁 → 2s timeout → Locked。Ok 即证早返回。
        assert!(
            save_runtime_state(&ws, &state).is_ok(),
            "deep-equal 应在取锁前返回"
        );
    }

    // 并发全流程 save(非仅 lock acquire):多线程存不同 state → 全 Ok + 最终文件合法 JSON + 无 tmp 残留。
    #[test]
    fn concurrent_full_flow_saves_serialize_without_corruption() {
        let ws = temp_ws();
        std::fs::create_dir_all(runtime_dir(&ws)).unwrap();
        let handles: Vec<_> = (0..6)
            .map(|t| {
                let w = ws.clone();
                std::thread::spawn(move || save_runtime_state(&w, &json!({ "t": t })))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap(); // 每个线程 save 都 Ok
        }
        let v = read_state(&ws); // 最终文件是合法 JSON(某个线程的 state)
        assert!(v["t"].is_number());
        let residue: Vec<_> = std::fs::read_dir(runtime_dir(&ws))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.ends_with(".tmp") || n.contains(".heal.")
            })
            .collect();
        assert!(residue.is_empty(), "无 tmp/heal 残留:{residue:?}");
    }

    // 锁串行化:持锁时第二个 acquire 在 timeout 内拿不到 → Locked。
    #[test]
    fn runtime_lock_serializes() {
        let ws = temp_ws();
        std::fs::create_dir_all(runtime_dir(&ws)).unwrap();
        let held = RuntimeLock::acquire(&ws, "state-save", 2.0).unwrap();
        // 另一线程在短 timeout 内尝试 → 应 Locked(flock 进程内/跨 fd 互斥)。
        let ws2 = ws.clone();
        let r = std::thread::spawn(move || RuntimeLock::acquire(&ws2, "state-save", 0.2))
            .join()
            .unwrap();
        assert!(
            matches!(r, Err(StateError::Locked(_))),
            "持锁时第二者应 Locked"
        );
        drop(held);
    }

    // ---- load 迁移(state.py:45/73/55)----

    #[test]
    fn normalize_agent_session_state_fills_fields_in_order() {
        let mut state = json!({"agents": {"w1": {"session_id": "keep"}}});
        normalize_agent_session_state(&mut state);
        let expected = json!({"agents": {"w1": {
            "session_id": "keep", "rollout_path": null, "captured_at": null,
            "captured_via": null, "attribution_confidence": null, "spawn_cwd": null,
        }}});
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
    }

    #[test]
    fn migrate_active_team_key_branches() {
        // session_name 存在 → seed = team_state_key。
        let mut m1 = json!({"session_name": "s", "team_dir": "/w/.team/tk", "teams": {}});
        assert!(migrate_active_team_key(&mut m1));
        assert_eq!(m1["active_team_key"], json!("tk"));
        // 单 team → 取唯一 key。
        let mut m2 = json!({"teams": {"only": {}}});
        assert!(migrate_active_team_key(&mut m2));
        assert_eq!(m2["active_team_key"], json!("only"));
        // 多 team 无 session → None。
        let mut m3 = json!({"teams": {"a": {}, "b": {}}});
        assert!(migrate_active_team_key(&mut m3));
        assert_eq!(m3["active_team_key"], json!(null));
        // 已有 active_team_key → 不迁移。
        let mut m4 = json!({"active_team_key": "x"});
        assert!(!migrate_active_team_key(&mut m4));
    }

    #[test]
    fn load_runtime_state_missing_returns_default() {
        let ws = temp_ws();
        let s = load_runtime_state(&ws).unwrap();
        assert_eq!(
            s,
            json!({"agents": {}, "tasks": [], "session_name": null, "active_team_key": null})
        );
    }

    #[test]
    fn load_runtime_state_migrates_and_persists() {
        // legacy state:有 session_name、无 active_team_key、agent 缺 session 字段、team_owner 缺 uuid。
        let ws = temp_ws();
        std::fs::create_dir_all(runtime_dir(&ws)).unwrap();
        let legacy = json!({
            "session_name": "sess",
            "team_dir": "/w/.team/tk",
            "agents": {"w1": {"agent_id": "w1"}},
            "team_owner": {"pane_id": "%1", "machine_fingerprint": "fp"},
        });
        std::fs::write(
            runtime_state_path(&ws),
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        let s = load_runtime_state(&ws).unwrap();
        // active_team_key seed = team_state_key = "tk"。
        assert_eq!(s["active_team_key"], json!("tk"));
        // agent session 字段补 None。
        assert_eq!(s["agents"]["w1"]["spawn_cwd"], json!(null));
        // team_owner 补 leader_session_uuid。
        assert_eq!(
            s["team_owner"]["leader_session_uuid"]
                .as_str()
                .unwrap()
                .len(),
            32
        );
        // 迁移已回写磁盘(再 load 不再变;active_team_key 已在)。
        let on_disk = read_state(&ws);
        assert_eq!(on_disk["active_team_key"], json!("tk"));
        assert_eq!(
            on_disk["team_owner"]["leader_session_uuid"],
            s["team_owner"]["leader_session_uuid"]
        );
    }

    // 对抗 P1:legacy 文件**已有 active_team_key** 但缺 leader_session_uuid。load 内存补 uuid,
    // 但 save 对 on-disk existing 同样 normalize+migrate 后发现等价 → **不重写**,磁盘字节恒等。
    // (此前 save 比 raw existing 漏了这两步 → spurious 重写成 pretty 形,破坏 load+save 字节恒等。)
    #[test]
    fn load_does_not_rewrite_already_migrated_equivalent_legacy() {
        let ws = temp_ws();
        std::fs::create_dir_all(runtime_dir(&ws)).unwrap();
        let legacy = r#"{"active_team_key": "preset", "session_name": "sess", "team_owner": {"pane_id": "%1"}}"#;
        std::fs::write(runtime_state_path(&ws), legacy).unwrap();
        let before = std::fs::read_to_string(runtime_state_path(&ws)).unwrap();
        let loaded = load_runtime_state(&ws).unwrap();
        // 内存态补了 uuid(证明确实需要迁移)。
        assert_eq!(
            loaded["team_owner"]["leader_session_uuid"]
                .as_str()
                .unwrap()
                .len(),
            32
        );
        // 但磁盘未被重写(字节恒等)。
        let after = std::fs::read_to_string(runtime_state_path(&ws)).unwrap();
        assert_eq!(
            after, before,
            "已是迁移等价形的 legacy 文件不得 spurious 重写"
        );
    }

    // Phase C: persist no longer repairs stale topology by cloning missing rows.
    // A stale non-lifecycle save that would remove a live row must surface a
    // typed conflict and leave disk unchanged.
    #[test]
    fn stale_snapshot_cannot_overwrite_new_topology() {
        let ws = temp_ws();
        let latest = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "w1",
                    "window": "w1-new",
                    "pane_id": "%2",
                    "pane_pid": 222,
                    "spawned_at": "2026-06-01T00:00:00Z",
                    "spawn_epoch": 2
                }
            },
        });
        write_state(&ws, &latest);
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "w1",
                    "window": "w1-old",
                    "pane_id": "%1",
                    "pane_pid": 111,
                    "spawned_at": "2026-05-31T00:00:00Z",
                    "spawn_epoch": 1
                }
            },
        });
        let err = save_runtime_state(&ws, &incoming).expect_err("stale topology must conflict");
        assert!(matches!(err, StateError::SaveConflict(_)));
        let message = err.to_string();
        assert!(message.contains("agent_id=w1"), "message={message}");
        assert!(message.contains("projection=agents"), "message={message}");
        assert!(message.contains("pane_id"), "message={message}");
        assert_eq!(
            read_state(&ws),
            latest,
            "conflict must leave disk unchanged"
        );
    }

    #[test]
    fn vacant_roster_preserve_cannot_resurrect_live_topology() {
        let ws = temp_ws();
        let latest = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": {"status": "running", "provider": "codex", "agent_id": "w1"},
                "joiner": {
                    "status": "running",
                    "provider": "codex",
                    "agent_id": "joiner",
                    "window": "joiner",
                    "pane_id": "%9",
                    "spawned_at": "2026-06-01T00:00:00Z",
                    "spawn_epoch": 1
                }
            },
        });
        write_state(&ws, &latest);
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": { "w1": {"status": "running", "provider": "codex", "agent_id": "w1"} },
        });
        let err = save_runtime_state(&ws, &incoming).expect_err("missing live row must conflict");
        assert!(matches!(err, StateError::SaveConflict(_)));
        assert!(err.to_string().contains("agent_id=joiner"));
    }

    #[test]
    fn vacant_non_live_roster_preserve_is_allow_list_only() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "agents": {
                    "typed": {
                        "agent_id": "typed",
                        "provider": "codex",
                        "role": "Developer",
                        "model": "gpt-5.5",
                        "status": "running",
                        "spawn_cwd": "/tmp/old",
                        "session_id": "old-session",
                        "rollout_path": "/tmp/old.jsonl"
                    }
                },
            }),
        );
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {},
        });
        save_runtime_state(&ws, &incoming).unwrap();
        let saved = read_state(&ws);
        let typed = saved.pointer("/agents/typed").expect("typed stub");
        assert_eq!(typed.get("provider").and_then(Value::as_str), Some("codex"));
        assert_eq!(typed.get("role").and_then(Value::as_str), Some("Developer"));
        assert_eq!(typed.get("model").and_then(Value::as_str), Some("gpt-5.5"));
        for forbidden in ["status", "spawn_cwd", "session_id", "rollout_path"] {
            assert!(
                typed.get(forbidden).is_none(),
                "typed roster stub must not copy {forbidden}; typed={typed}"
            );
        }
    }

    #[test]
    fn reapplying_after_conflict_retries_without_losing_delta() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "agents": {
                    "w1": {
                        "agent_id": "w1",
                        "provider": "codex",
                        "window": "w1-new",
                        "pane_id": "%2",
                        "pane_pid": 222,
                        "spawn_epoch": 2
                    }
                },
                "delivery": {}
            }),
        );
        let stale_with_delta = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "w1": {
                    "agent_id": "w1",
                    "provider": "codex",
                    "window": "w1-old",
                    "pane_id": "%1",
                    "pane_pid": 111,
                    "spawn_epoch": 1
                }
            },
            "delivery": {"msg-1": {"status": "delivered"}}
        });
        let mut reapplied = false;
        save_runtime_state_reapplying_after_conflict(&ws, &stale_with_delta, |latest| {
            reapplied = true;
            latest.as_object_mut().unwrap().insert(
                "delivery".to_string(),
                json!({"msg-1": {"status": "delivered"}}),
            );
        })
        .unwrap();

        let saved = read_state(&ws);
        assert!(reapplied, "SaveConflict path must reload and reapply once");
        assert_eq!(saved.pointer("/agents/w1/pane_id"), Some(&json!("%2")));
        assert_eq!(saved.pointer("/agents/w1/window"), Some(&json!("w1-new")));
        assert_eq!(
            saved.pointer("/delivery/msg-1/status"),
            Some(&json!("delivered")),
            "non-topology delta must survive retry; saved={saved}"
        );
    }

    #[test]
    fn deleted_ids_stay_dead_even_when_latest_has_live_topology() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "agents": {
                    "gone": {
                        "status": "running",
                        "provider": "codex",
                        "agent_id": "gone",
                        "window": "gone",
                        "pane_id": "%7",
                        "spawn_epoch": 1
                    },
                },
            }),
        );
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {},
        });
        save_runtime_state_with_deleted_agents(&ws, &incoming, &["gone"]).unwrap();
        let saved = read_state(&ws);
        assert!(
            saved.pointer("/agents/gone").is_none(),
            "deleted_agent_ids 豁免:被 remove 的 `gone` 不得被 preserve 复活;saved={saved}"
        );
    }

    #[test]
    fn lifecycle_topology_authority_only_applies_to_target_agent() {
        let ws = temp_ws();
        let latest = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "target": {"agent_id": "target", "provider": "codex", "window": "target", "pane_id": "%1", "spawn_epoch": 1},
                "other": {"agent_id": "other", "provider": "codex", "window": "other", "pane_id": "%2", "spawn_epoch": 1}
            },
        });
        write_state(&ws, &latest);
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "target": {"agent_id": "target", "provider": "codex", "status": "stopped"},
                "other": {"agent_id": "other", "provider": "codex", "window": "old-other", "pane_id": "%old", "spawn_epoch": 1}
            },
        });
        let err = save_runtime_state_with_lifecycle_topology_authority(&ws, &incoming, &["target"])
            .expect_err("authorized target must not hide other agent conflict");
        assert!(matches!(err, StateError::SaveConflict(_)));
        assert!(err.to_string().contains("agent_id=other"));
    }

    #[test]
    fn lifecycle_topology_authority_can_clear_target_topology() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "agents": {
                    "target": {"agent_id": "target", "provider": "codex", "window": "target", "pane_id": "%1", "spawn_epoch": 1}
                },
            }),
        );
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "target": {"agent_id": "target", "provider": "codex", "status": "stopped"}
            },
        });
        save_runtime_state_with_lifecycle_topology_authority(&ws, &incoming, &["target"]).unwrap();
        let target = read_state(&ws).pointer("/agents/target").cloned().unwrap();
        assert!(
            target.get("pane_id").is_none(),
            "target topology was intentionally cleared: {target}"
        );
        assert!(
            target.get("window").is_none(),
            "target window was intentionally cleared: {target}"
        );
    }

    #[test]
    fn owner_epoch_preserve_is_explicit_and_bounded_to_team_entry() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "teams": {
                    "team-a": {
                        "agents": {},
                        "team_owner": {"pane_id": "%7", "owner_epoch": 7},
                        "leader_receiver": {"pane_id": "%7", "owner_epoch": 7},
                        "owner_epoch": 7
                    }
                }
            }),
        );
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "teams": {
                "team-a": {
                    "agents": {},
                    "team_owner": {"pane_id": "%3", "owner_epoch": 3},
                    "leader_receiver": {"pane_id": "%3", "owner_epoch": 3},
                    "owner_epoch": 3
                }
            }
        });
        save_runtime_state(&ws, &incoming).unwrap();
        let saved = read_state(&ws);
        assert_eq!(
            saved
                .pointer("/teams/team-a/owner_epoch")
                .and_then(Value::as_u64),
            Some(7)
        );
        assert_eq!(
            saved
                .pointer("/teams/team-a/team_owner/pane_id")
                .and_then(Value::as_str),
            Some("%7")
        );
        assert!(
            saved.get("team_owner").is_none(),
            "top-level owner must not be recreated: {saved}"
        );
        assert!(
            saved.get("owner_epoch").is_none(),
            "top-level owner_epoch must not be recreated: {saved}"
        );
    }

    #[test]
    fn projection_paths_do_not_cross_backfill_topology() {
        let ws = temp_ws();
        write_state(
            &ws,
            &json!({
                "session_name": "team-a",
                "active_team_key": "team-a",
                "agents": {
                    "top-only": {"agent_id": "top-only", "provider": "codex", "window": "top-only", "pane_id": "%1", "spawn_epoch": 1}
                },
                "teams": {
                    "team-a": {
                        "agents": {
                            "team-only": {"agent_id": "team-only", "provider": "codex", "window": "team-only", "pane_id": "%2", "spawn_epoch": 1}
                        }
                    }
                }
            }),
        );
        let incoming = json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {
                "top-only": {"agent_id": "top-only", "provider": "codex", "window": "top-only", "pane_id": "%1", "spawn_epoch": 1}
            },
            "teams": {
                "team-a": {
                    "agents": {
                        "team-only": {"agent_id": "team-only", "provider": "codex", "window": "team-only", "pane_id": "%2", "spawn_epoch": 1}
                    }
                }
            }
        });
        save_runtime_state(&ws, &incoming).unwrap();
        let saved = read_state(&ws);
        assert!(
            saved.pointer("/agents/team-only").is_none(),
            "teams.<key> topology must not cross-fill top-level agents; saved={saved}"
        );
        assert!(
            saved.pointer("/teams/team-a/agents/top-only").is_none(),
            "top-level topology must not cross-fill teams.<key>.agents; saved={saved}"
        );
    }

    /// RM-039-STAT-001 second-round regression (architect verdict
    /// 2026-06-22): `migrate_team_key_to_match_active_team` must
    /// promote `team_key = active_team_key` when root `team_key` is
    /// missing AND `teams[active_team_key]` exists, and mirror it into
    /// the team-scoped slot. Coordinator tick then writes to the same
    /// teams entry that status selector reads.
    #[test]
    fn migrate_team_key_to_match_active_team_promotes_when_missing() {
        let mut state = json!({
            "active_team_key": "rm039-status-working-891",
            "session_name": "team-rm039-status-working",
            "team_dir": "./.team/current",
            "teams": {
                "rm039-status-working-891": {
                    "active_team_key": "rm039-status-working-891",
                    "session_name": "team-rm039-status-working",
                    "agents": {}
                }
            }
        });
        let changed = migrate_team_key_to_match_active_team(&mut state);
        assert!(changed, "state must be mutated when team_key is missing");
        assert_eq!(
            state.get("team_key").and_then(Value::as_str),
            Some("rm039-status-working-891"),
            "root team_key must equal active_team_key after migration"
        );
        // mirrored into the team-scoped entry
        assert_eq!(
            state
                .pointer("/teams/rm039-status-working-891/team_key")
                .and_then(Value::as_str),
            Some("rm039-status-working-891"),
            "teams[active].team_key must be set as part of the migration"
        );
        // team_state_key cascade now agrees with active_team_key.
        assert_eq!(
            crate::state::projection::team_state_key(&state),
            "rm039-status-working-891",
            "team_state_key first branch must hit `state.team_key` and \
             return the canonical key"
        );
    }

    /// Conflict cases stay observable: if `team_key` already exists,
    /// the migrator does NOT silently overwrite it, even when it
    /// disagrees with `active_team_key`. The user/team can resolve.
    #[test]
    fn migrate_team_key_to_match_active_team_is_noop_when_team_key_present() {
        let mut state = json!({
            "team_key": "explicit-key",
            "active_team_key": "different-active",
            "teams": {
                "different-active": {"agents": {}},
                "explicit-key": {"agents": {}}
            }
        });
        let changed = migrate_team_key_to_match_active_team(&mut state);
        assert!(!changed, "existing team_key must NOT be overwritten");
        assert_eq!(
            state.get("team_key").and_then(Value::as_str),
            Some("explicit-key")
        );
    }

    /// Narrow: only promote when teams[active_team_key] actually exists.
    /// Otherwise we'd be claiming a key for a non-existent team slot.
    #[test]
    fn migrate_team_key_to_match_active_team_is_noop_when_active_team_entry_missing() {
        let mut state = json!({
            "active_team_key": "missing-team",
            "teams": {
                "other": {"agents": {}}
            }
        });
        let changed = migrate_team_key_to_match_active_team(&mut state);
        assert!(
            !changed,
            "no migration when teams[active_team_key] does not exist"
        );
        assert!(state.get("team_key").is_none());
    }

    /// Null / empty active_team_key is not actionable.
    #[test]
    fn migrate_team_key_to_match_active_team_is_noop_when_active_team_key_absent() {
        for active in [json!(null), json!(""), json!(serde_json::Value::Null)] {
            let mut state = json!({
                "active_team_key": active,
                "teams": {}
            });
            assert!(
                !migrate_team_key_to_match_active_team(&mut state),
                "no migration when active_team_key is null/empty"
            );
        }
    }

    /// RM-039-STAT-001 second-round end-to-end at the persistence layer
    /// (architect verdict 2026-06-22): loading a state file in the dirty
    /// shape (active_team_key disagrees with team_dir basename, root
    /// team_key absent) must persist the canonical `team_key` so the
    /// next `save_team_scoped_state` writes the team-scoped slot at
    /// `teams[active_team_key]`, not at `teams[team_dir_basename]`.
    /// This is the bridging contract that decides whether coordinator
    /// tick's activity write lands where the status selector later reads.
    #[test]
    fn load_runtime_state_pins_team_key_from_active_team_key_on_dirty_shape() {
        let ws = temp_ws();
        let active = "rm039-status-working-891";
        let raw = json!({
            "session_name": "team-rm039-status-working",
            "team_dir": "./.team/current",
            "active_team_key": active,
            // intentionally NO root "team_key" — the dirty shape.
            "agents": {
                "coder": {"status": "running", "first_send_at": "2026-01-01T00:00:00Z"}
            },
            "teams": {
                "current": {
                    "active_team_key": active,
                    "agents": {"coder": {"status": "running"}}
                },
                active: {
                    "active_team_key": active,
                    "agents": {"coder": {"status": "running"}}
                }
            }
        });
        let runtime_dir = ws.join(".team").join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            runtime_dir.join("state.json"),
            serde_json::to_vec_pretty(&raw).unwrap(),
        )
        .unwrap();
        // temp_ws() returns a unique path per call, so the cache
        // (keyed by path) does not collide with previous tests.

        let loaded = load_runtime_state(&ws).expect("loaded state");
        assert_eq!(
            loaded.get("team_key").and_then(Value::as_str),
            Some(active),
            "load_runtime_state must promote team_key=active_team_key on the dirty shape"
        );
        // team_state_key now cascades to state.team_key, returning the
        // canonical key instead of the team_dir basename.
        assert_eq!(
            crate::state::projection::team_state_key(&loaded),
            active,
            "team_state_key must return the canonical key after migration"
        );

        // Re-read from disk (not cache) to confirm the migration was persisted.
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(runtime_dir.join("state.json")).unwrap())
                .unwrap();
        assert_eq!(
            on_disk.get("team_key").and_then(Value::as_str),
            Some(active),
            "migration must be persisted to disk so subsequent processes see it"
        );
    }

    /// 0.4.6 tuple-atomic contract test #1 (audit §Required Tests, line 264):
    /// latest has `session_id` only (PARTIAL TUPLE), incoming has null
    /// tuple → no backfill. This is bug-045's core failure mode: persist
    /// must not resurrect a scalar session_id when the latest row's tuple
    /// is incomplete.
    #[test]
    fn backfill_capture_skips_when_latest_tuple_is_partial() {
        let mut incoming = json!({
            "session_id": Value::Null,
            "rollout_path": Value::Null,
            "captured_at": Value::Null,
            "captured_via": Value::Null,
        });
        let latest = json!({
            "session_id": "partial-only-uuid",
            "rollout_path": Value::Null,
            "captured_at": Value::Null,
            "captured_via": Value::Null,
        });
        backfill_capture_fields(&mut incoming, &latest);
        assert_eq!(
            incoming.get("session_id"),
            Some(&Value::Null),
            "0.4.6: partial latest tuple must NOT backfill session_id; got {incoming}"
        );
    }

    /// 0.4.6 tuple-atomic contract test #2 (audit, line 265):
    /// latest has complete tuple, incoming has null tuple → all 4 fields
    /// backfilled together. This is the concurrent-capture protection
    /// (A0/R2) that must still work after the tuple-atomic rewrite.
    #[test]
    fn backfill_capture_atomically_copies_complete_tuple() {
        let mut incoming = json!({
            "session_id": Value::Null,
            "rollout_path": Value::Null,
            "captured_at": Value::Null,
            "captured_via": Value::Null,
        });
        let latest = json!({
            "session_id": "real-uuid",
            "rollout_path": "/tmp/real.jsonl",
            "captured_at": "2026-06-25T10:00:00+00:00",
            "captured_via": "session.captured",
            "attribution_confidence": "high",
        });
        backfill_capture_fields(&mut incoming, &latest);
        assert_eq!(
            incoming.get("session_id").and_then(Value::as_str),
            Some("real-uuid")
        );
        assert_eq!(
            incoming.get("rollout_path").and_then(Value::as_str),
            Some("/tmp/real.jsonl")
        );
        assert_eq!(
            incoming.get("captured_at").and_then(Value::as_str),
            Some("2026-06-25T10:00:00+00:00")
        );
        assert_eq!(
            incoming.get("captured_via").and_then(Value::as_str),
            Some("session.captured")
        );
        assert_eq!(
            incoming
                .get("attribution_confidence")
                .and_then(Value::as_str),
            Some("high"),
            "attribution_confidence rides with the tuple"
        );
    }

    /// 0.4.6 tuple-atomic contract test #3 (audit, line 266):
    /// latest has complete tuple for one session_id, incoming has non-null
    /// DIFFERENT session_id → no mixed tuple. Prevents one writer's
    /// session_id getting glued to another writer's rollout_path/captured_at.
    #[test]
    fn backfill_capture_refuses_mixing_different_session_ids() {
        let mut incoming = json!({
            "session_id": "incoming-different-uuid",
            "rollout_path": Value::Null,
            "captured_at": Value::Null,
            "captured_via": Value::Null,
        });
        let latest = json!({
            "session_id": "latest-uuid",
            "rollout_path": "/tmp/latest.jsonl",
            "captured_at": "2026-06-25T10:00:00+00:00",
            "captured_via": "session.captured",
        });
        backfill_capture_fields(&mut incoming, &latest);
        // session_id stays as incoming (not overwritten).
        assert_eq!(
            incoming.get("session_id").and_then(Value::as_str),
            Some("incoming-different-uuid"),
            "0.4.6: incoming session_id must not be overwritten by a different latest"
        );
        // Sibling fields stay null (no cross-session mixing).
        assert_eq!(
            incoming.get("rollout_path"),
            Some(&Value::Null),
            "0.4.6: must not copy latest rollout_path onto a different session_id"
        );
    }

    /// 0.4.6 tuple-atomic contract test #4 (audit, line 267):
    /// `normalize_agent_session_state` may only insert null defaults — it
    /// must never create non-null session truth.
    #[test]
    fn normalize_agent_session_state_only_inserts_null_defaults() {
        let mut state = json!({
            "agents": {
                "alpha": {}
            }
        });
        normalize_agent_session_state(&mut state);
        let alpha = state.pointer("/agents/alpha").expect("alpha");
        for field in [
            "session_id",
            "rollout_path",
            "captured_at",
            "captured_via",
            "attribution_confidence",
            "spawn_cwd",
        ] {
            assert_eq!(
                alpha.get(field),
                Some(&Value::Null),
                "normalize must insert null for {field}; got {alpha}"
            );
        }
    }

    /// Bug 045 end-to-end persist regression: poisoned latest (session_id
    /// but no captured_at/captured_via) cannot resurrect a cleared incoming
    /// row through a real save → reload cycle.
    #[test]
    fn poisoned_partial_latest_does_not_resurrect_on_save_reload() {
        let ws = temp_ws();
        // Seed disk with the exact bug-045 poisoned shape: session_id set,
        // no captured_at / captured_via / rollout_path.
        let poisoned = json!({
            "session_name": "team-bug045-poisoned",
            "team_key": "teamP",
            "agents": {
                "alpha": {
                    "status": "stopped",
                    "provider": "claude",
                    "session_id": "stale-poisoned-uuid",
                    "rollout_path": Value::Null,
                    "captured_at": Value::Null,
                    "captured_via": Value::Null
                }
            }
        });
        save_runtime_state(&ws, &poisoned).unwrap();

        // Restart-fresh writes a cleared row.
        let cleared = json!({
            "session_name": "team-bug045-poisoned",
            "team_key": "teamP",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "claude",
                    "session_id": Value::Null,
                    "rollout_path": Value::Null,
                    "captured_at": Value::Null,
                    "captured_via": Value::Null
                }
            }
        });
        save_runtime_state(&ws, &cleared).unwrap();

        let on_disk = read_state(&ws);
        let alpha = on_disk.pointer("/agents/alpha").expect("alpha");
        assert_eq!(
            alpha.get("session_id"),
            Some(&Value::Null),
            "0.4.6: partial poisoned latest must not resurrect cleared session_id; got {alpha}"
        );
    }
}
