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

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// `state.py:_RUNTIME_STATE_CACHE`:进程级 path→state 缓存(deep-equal 早返回)。
static RUNTIME_STATE_CACHE: LazyLock<Mutex<HashMap<PathBuf, Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `state.py:41`。
pub fn runtime_state_path(workspace: &Path) -> PathBuf {
    runtime_dir(workspace).join("state.json")
}

fn cache_equals(path: &Path, state: &Value) -> bool {
    RUNTIME_STATE_CACHE.lock().is_ok_and(|c| c.get(path) == Some(state))
}
fn cache_set(path: &Path, state: &Value) {
    if let Ok(mut c) = RUNTIME_STATE_CACHE.lock() {
        c.insert(path.to_path_buf(), state.clone());
    }
}
/// `_RUNTIME_STATE_CACHE.get(...)` → `copy.deepcopy(cached)`(clone = deepcopy)。
fn cache_get(path: &Path) -> Option<Value> {
    RUNTIME_STATE_CACHE.lock().ok().and_then(|c| c.get(path).cloned())
}

fn unique_tmp(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
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
/// POSIX flock(unix);Windows 锁(LockFileEx)延平台层(step 9+)。
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
        let file = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            let start = Instant::now();
            loop {
                // SAFETY: fd 来自打开的 lock_file,LOCK_EX|LOCK_NB 非阻塞。
                let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    return Ok(Self { file });
                }
                if start.elapsed().as_secs_f64() >= timeout {
                    return Err(StateError::Locked(name.to_string()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = timeout;
            Err(StateError::Locked(format!("{name} (runtime lock not yet implemented on non-unix)")))
        }
    }
}

#[cfg(unix)]
impl Drop for RuntimeLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: 释放本进程持有的 flock。
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// `save_runtime_state`(bug-084)。`state` 是 state.json 的内存 Value(插入序保留)。
/// 注:Python 在此还调 `_migrate_state_identity`(identity slice 落地后接入;本 slice 不改 state 内容)。
pub fn save_runtime_state(workspace: &Path, state: &Value) -> Result<(), StateError> {
    let path = runtime_state_path(workspace);
    if cache_equals(&path, state) {
        return Ok(());
    }
    // Python `state.py:497`:先对入参 state 跑 `_migrate_state_identity`(就地填缺失 leader uuid)。
    // 我们 `&Value` 不可变 → 克隆后迁移,后续比较/写入/缓存/self-heal 全走 `migrated`。
    // 该步**不**包 try/except → 错误 propagate(对齐 Python)。
    let mut migrated = state.clone();
    migrate_state_identity(&mut migrated, &SystemEnv, workspace)?;
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
    Err(StateError::SaveFailed("retry loop exhausted without return".to_string()))
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
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
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
    if state.as_object().is_some_and(|o| o.contains_key("active_team_key")) {
        return false;
    }
    let teams_is_dict = state.get("teams").is_some_and(Value::is_object);
    let teams_len = state.get("teams").and_then(Value::as_object).map_or(0, serde_json::Map::len);
    if state.get("session_name").is_some_and(json_truthy) {
        let seed = team_state_key(state);
        if let Some(o) = state.as_object_mut() {
            o.insert("active_team_key".to_string(), Value::String(seed));
        }
        return true;
    }
    if teams_is_dict && teams_len == 1 {
        let first = state.get("teams").and_then(Value::as_object).and_then(|t| t.keys().next().cloned());
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

pub fn load_runtime_state(workspace: &Path) -> Result<Value, StateError> {
    let path = runtime_state_path(workspace);
    if !path.exists() {
        if let Some(cached) = cache_get(&path) {
            return Ok(cached);
        }
        return Ok(json!({"agents": {}, "tasks": [], "session_name": null, "active_team_key": null}));
    }
    let text = std::fs::read_to_string(&path)?;
    let mut state: Value = serde_json::from_str(&text)?;
    normalize_agent_session_state(&mut state);
    let mut changed = migrate_state_identity(&mut state, &SystemEnv, workspace)?;
    if migrate_active_team_key(&mut state) {
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
        read_events(ws).iter().filter(|e| e["event"] == json!(name)).count()
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
        read_events(ws).into_iter().find(|e| e["event"] == json!(name)).unwrap_or(Value::Null)
    }
    fn read_state(ws: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(runtime_state_path(ws)).unwrap()).unwrap()
    }
    fn bak_files(ws: &Path) -> Vec<PathBuf> {
        let dir = runtime_dir(ws);
        std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains(".bak.")))
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
        assert_eq!(serde_json::to_string_pretty(&v).unwrap(), canonical, "state.json 序列化未字节对齐 Python indent=2");
    }

    #[test]
    fn save_writes_atomically_and_caches() {
        let ws = temp_ws();
        let state = json!({"session_name":"t","agents":{"a":{"agent_id":"a"}},"active_team_key":"t"});
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
        assert!(!runtime_state_path(&ws).exists(), "deep-equal 命中缓存 → 未重写(文件仍不存在)");
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
        let retries: Vec<_> = read_events(&ws).into_iter().filter(|e| e["event"] == json!("runtime.state.save_retry")).collect();
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
            assert_eq!(get_event(&ws, "runtime.state.save_retry")["errno_name"], json!(name));
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
        assert_eq!(count_event(&ws, "runtime.state.self_healed"), 0, "未触发 self-heal");
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
        assert_eq!(read_state(&ws), original, "restore 成功:原 state 复位到 state.json");
        assert_eq!(count_event(&ws, "runtime.state.self_heal_restore_failed"), 0);
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
        assert_eq!(count_event(&ws, "runtime.state.self_heal_restore_failed"), 1);
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
        assert!(save_runtime_state(&ws, &state).is_ok(), "deep-equal 应在取锁前返回");
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
        let r = std::thread::spawn(move || RuntimeLock::acquire(&ws2, "state-save", 0.2)).join().unwrap();
        assert!(matches!(r, Err(StateError::Locked(_))), "持锁时第二者应 Locked");
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
        assert_eq!(serde_json::to_string(&state).unwrap(), serde_json::to_string(&expected).unwrap());
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
        assert_eq!(s, json!({"agents": {}, "tasks": [], "session_name": null, "active_team_key": null}));
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
        std::fs::write(runtime_state_path(&ws), serde_json::to_string(&legacy).unwrap()).unwrap();
        let s = load_runtime_state(&ws).unwrap();
        // active_team_key seed = team_state_key = "tk"。
        assert_eq!(s["active_team_key"], json!("tk"));
        // agent session 字段补 None。
        assert_eq!(s["agents"]["w1"]["spawn_cwd"], json!(null));
        // team_owner 补 leader_session_uuid。
        assert_eq!(s["team_owner"]["leader_session_uuid"].as_str().unwrap().len(), 32);
        // 迁移已回写磁盘(再 load 不再变;active_team_key 已在)。
        let on_disk = read_state(&ws);
        assert_eq!(on_disk["active_team_key"], json!("tk"));
        assert_eq!(on_disk["team_owner"]["leader_session_uuid"], s["team_owner"]["leader_session_uuid"]);
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
        assert_eq!(loaded["team_owner"]["leader_session_uuid"].as_str().unwrap().len(), 32);
        // 但磁盘未被重写(字节恒等)。
        let after = std::fs::read_to_string(runtime_state_path(&ws)).unwrap();
        assert_eq!(after, before, "已是迁移等价形的 legacy 文件不得 spurious 重写");
    }
}
