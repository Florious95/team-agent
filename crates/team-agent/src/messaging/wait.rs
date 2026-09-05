use std::path::Path;

use serde_json::Value;

use super::MessagingError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitResult {
    pub task_id: String,
    pub result_id: String,
    pub waited: bool,
}

impl WaitResult {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "ok": true,
            "status": "completed",
            "task_id": self.task_id,
            "result_id": self.result_id,
            "waited": self.waited,
        })
    }
}

#[cfg(not(unix))]
pub fn wait_for_result(_workspace: &Path, _task_id: &str) -> Result<WaitResult, MessagingError> {
    Err(MessagingError::Validation(
        "wait --task requires POSIX FIFO support".to_string(),
    ))
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicI32, Ordering};

    use rusqlite::{params, OptionalExtension};

    use crate::event_log::EventLog;
    use crate::message_store::MessageStore;

    use super::{MessagingError, WaitResult};

    static CAUGHT_SIGNAL: AtomicI32 = AtomicI32::new(0);

    enum Registration {
        Completed(String),
        Pending,
    }

    struct FifoEndpoints {
        path: PathBuf,
        reader: File,
        _keepalive_writer: File,
    }

    impl Drop for FifoEndpoints {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    struct RegistrationCleanup {
        db_path: PathBuf,
        watcher_id: String,
        active: bool,
    }

    impl RegistrationCleanup {
        fn cleanup(&mut self) -> Result<(), MessagingError> {
            if self.active {
                let conn = crate::db::schema::open_db(&self.db_path)?;
                conn.execute(
                    "delete from result_watchers where watcher_id = ?1",
                    params![self.watcher_id],
                )?;
                self.active = false;
            }
            Ok(())
        }
    }

    impl Drop for RegistrationCleanup {
        fn drop(&mut self) {
            if self.active {
                if let Ok(conn) = crate::db::schema::open_db(&self.db_path) {
                    let _ = conn.execute(
                        "delete from result_watchers where watcher_id = ?1",
                        params![self.watcher_id],
                    );
                }
            }
        }
    }

    struct SignalGuard {
        old_int: libc::sigaction,
        old_term: libc::sigaction,
    }

    impl SignalGuard {
        fn install() -> io::Result<Self> {
            CAUGHT_SIGNAL.store(0, Ordering::SeqCst);
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = record_signal as *const () as libc::sighandler_t;
            action.sa_flags = 0;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            let mut old_int: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old_term: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(libc::SIGINT, &action, &mut old_int) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::sigaction(libc::SIGTERM, &action, &mut old_term) } != 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::sigaction(libc::SIGINT, &old_int, std::ptr::null_mut());
                }
                return Err(error);
            }
            Ok(Self { old_int, old_term })
        }
    }

    impl Drop for SignalGuard {
        fn drop(&mut self) {
            unsafe {
                libc::sigaction(libc::SIGINT, &self.old_int, std::ptr::null_mut());
                libc::sigaction(libc::SIGTERM, &self.old_term, std::ptr::null_mut());
            }
        }
    }

    extern "C" fn record_signal(signal: libc::c_int) {
        CAUGHT_SIGNAL.store(signal, Ordering::SeqCst);
    }

    pub fn wait_for_result(workspace: &Path, task_id: &str) -> Result<WaitResult, MessagingError> {
        if task_id.trim().is_empty() {
            return Err(MessagingError::Validation(
                "wait --task requires a non-empty task id".to_string(),
            ));
        }
        let _signals = SignalGuard::install()?;
        let store = MessageStore::open(workspace)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let watcher_id = format!("wait-{}", random_suffix()?);
        let fifo_path = fifo_path(&workspace, task_id, &watcher_id)?;
        let fifo = create_fifo(&fifo_path)?;
        let conn = crate::db::schema::open_db(store.db_path())?;
        let registration = register(&conn, &watcher_id, task_id, &fifo_path)?;
        if let Registration::Completed(result_id) = registration {
            return Ok(WaitResult {
                task_id: task_id.to_string(),
                result_id,
                waited: false,
            });
        }

        let mut cleanup = RegistrationCleanup {
            db_path: store.db_path().to_path_buf(),
            watcher_id: watcher_id.clone(),
            active: true,
        };
        EventLog::new(&workspace).write(
            "result_wake.registered",
            serde_json::json!({
                "task_id": task_id,
                "watcher_id": watcher_id,
                "recipient": fifo_path,
            }),
        )?;
        let result_id = read_wake_line(fifo.reader.as_raw_fd())?;
        cleanup.cleanup()?;
        Ok(WaitResult {
            task_id: task_id.to_string(),
            result_id,
            waited: true,
        })
    }

    fn register(
        conn: &rusqlite::Connection,
        watcher_id: &str,
        task_id: &str,
        fifo_path: &Path,
    ) -> Result<Registration, MessagingError> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let operation = (|| -> Result<Registration, MessagingError> {
            let completed = conn
                .query_row(
                    "select result_id from results
                     where task_id = ?1 order by created_at, result_id limit 1",
                    params![task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(result_id) = completed {
                return Ok(Registration::Completed(result_id));
            }
            conn.execute(
                "insert into result_watchers(
                     watcher_id, owner_team_id, task_id, agent_id, message_id,
                     leader_id, recipient, status, created_at
                 ) values (?1, null, ?2, null, null, 'team-agent', ?3, 'pending', ?4)",
                params![
                    watcher_id,
                    task_id,
                    fifo_path.to_string_lossy().as_ref(),
                    chrono::Utc::now().to_rfc3339(),
                ],
            )?;
            Ok(Registration::Pending)
        })();
        match operation {
            Ok(registration) => match conn.execute_batch("COMMIT") {
                Ok(()) => Ok(registration),
                Err(error) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    Err(error.into())
                }
            },
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn fifo_path(
        workspace: &Path,
        task_id: &str,
        watcher_id: &str,
    ) -> Result<PathBuf, MessagingError> {
        let dir = workspace.join(".team/runtime/wake");
        std::fs::create_dir_all(&dir)?;
        let task = task_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .take(80)
            .collect::<String>();
        let task = if task.is_empty() { "task" } else { &task };
        Ok(dir.join(format!("{task}-{watcher_id}.fifo")))
    }

    fn random_suffix() -> Result<String, MessagingError> {
        let mut bytes = [0_u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn create_fifo(path: &Path) -> Result<FifoEndpoints, MessagingError> {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| MessagingError::Validation("FIFO path contains a NUL byte".to_string()))?;
        if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let reader = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(reader) => reader,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(error.into());
            }
        };
        let keepalive_writer = match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(writer) => writer,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(error.into());
            }
        };
        set_blocking(reader.as_raw_fd())?;
        Ok(FifoEndpoints {
            path: path.to_path_buf(),
            reader,
            _keepalive_writer: keepalive_writer,
        })
    }

    fn set_blocking(fd: RawFd) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn read_wake_line(fd: RawFd) -> Result<String, MessagingError> {
        let mut line = Vec::new();
        loop {
            if let Some(signal) = caught_signal() {
                return Err(interrupted(signal).into());
            }
            let mut bytes = [0_u8; 256];
            let count =
                unsafe { libc::read(fd, bytes.as_mut_ptr().cast::<libc::c_void>(), bytes.len()) };
            if count > 0 {
                line.extend_from_slice(&bytes[..count as usize]);
                if let Some(newline) = line.iter().position(|byte| *byte == b'\n') {
                    line.truncate(newline);
                    let result_id = String::from_utf8(line).map_err(|error| {
                        MessagingError::Validation(format!("invalid FIFO wake line: {error}"))
                    })?;
                    let result_id = result_id.trim().to_string();
                    if result_id.is_empty() {
                        return Err(MessagingError::Validation(
                            "FIFO wake line did not contain a result id".to_string(),
                        ));
                    }
                    return Ok(result_id);
                }
                continue;
            }
            if count == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                if let Some(signal) = caught_signal() {
                    return Err(interrupted(signal).into());
                }
                continue;
            }
            return Err(error.into());
        }
    }

    fn caught_signal() -> Option<i32> {
        let signal = CAUGHT_SIGNAL.load(Ordering::SeqCst);
        (signal != 0).then_some(signal)
    }

    fn interrupted(signal: i32) -> io::Error {
        io::Error::new(
            io::ErrorKind::Interrupted,
            format!("wait interrupted by signal {signal}"),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn workspace(tag: &str) -> PathBuf {
            let path = std::env::temp_dir().join(format!(
                "team-agent-wait-{tag}-{}",
                crate::messaging::helpers::next_run_id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn envelope(result_id: &str, task_id: &str) -> serde_json::Value {
            serde_json::json!({
                "schema_version":"result_envelope_v1",
                "result_id":result_id,
                "task_id":task_id,
                "agent_id":"worker",
                "status":"success",
                "summary":"done",
                "changes":[],"tests":[],"risks":[],"artifacts":[],"next_actions":[]
            })
        }

        #[test]
        fn fifo_result_before_register_returns_without_waiting() {
            let ws = workspace("result-first");
            crate::messaging::report_result_for_owner_team(
                &ws,
                &envelope("res-first", "original"),
                Some("teamA"),
            )
            .unwrap();
            let store = MessageStore::open(&ws).unwrap();
            let conn = crate::db::schema::open_db(store.db_path()).unwrap();
            let fifo_path = fifo_path(
                &std::fs::canonicalize(&ws).unwrap(),
                "original",
                "watch-first",
            )
            .unwrap();
            let _fifo = create_fifo(&fifo_path).unwrap();
            assert!(
                matches!(register(&conn, "watch-first", "original", &fifo_path).unwrap(), Registration::Completed(id) if id == "res-first")
            );
            assert_eq!(
                conn.query_row(
                    "select count(*) from result_watchers where watcher_id='watch-first'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }

        #[test]
        fn fifo_register_before_result_wakes_original_once_only() {
            let ws = workspace("register-first");
            let store = MessageStore::open(&ws).unwrap();
            let conn = crate::db::schema::open_db(store.db_path()).unwrap();
            let canonical = std::fs::canonicalize(&ws).unwrap();
            let original_path = fifo_path(&canonical, "original", "watch-original").unwrap();
            let nested_path = fifo_path(&canonical, "nested", "watch-nested").unwrap();
            let original_fifo = create_fifo(&original_path).unwrap();
            let _nested_fifo = create_fifo(&nested_path).unwrap();
            assert!(matches!(
                register(&conn, "watch-original", "original", &original_path).unwrap(),
                Registration::Pending
            ));
            assert!(matches!(
                register(&conn, "watch-nested", "nested", &nested_path).unwrap(),
                Registration::Pending
            ));

            crate::messaging::report_result_for_owner_team(
                &ws,
                &envelope("res-once", "original"),
                Some("teamA"),
            )
            .unwrap();
            assert_eq!(
                read_wake_line(original_fifo.reader.as_raw_fd()).unwrap(),
                "res-once"
            );
            let duplicate = crate::messaging::report_result_for_owner_team(
                &ws,
                &envelope("res-once", "original"),
                Some("teamA"),
            )
            .unwrap();
            assert_eq!(duplicate["status"], serde_json::json!("duplicate_ignored"));
            assert_eq!(conn.query_row("select count(*) from result_watchers where watcher_id='watch-nested' and status='pending'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
            let notifications = EventLog::new(&ws)
                .tail(0)
                .unwrap()
                .into_iter()
                .filter(|event| {
                    event["event"] == serde_json::json!("result_wake.notified")
                        && event["task_id"] == serde_json::json!("original")
                })
                .count();
            assert_eq!(notifications, 1);
        }
    }
}

#[cfg(unix)]
pub use unix::wait_for_result;
