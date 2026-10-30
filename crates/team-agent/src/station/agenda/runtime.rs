//! Event-driven agenda loop: the only idle timer is the sixty-second exit grace.
use super::{supervisor, Store};
use serde_json::{json, Value};
use std::path::Path;

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::station::seat_store;
    use chrono::Utc;
    use std::collections::HashSet;
    use std::os::unix::net::UnixDatagram;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const IDLE: Duration = Duration::from_secs(60);
    const WORKERS: usize = 4;
    fn socket_dir() -> Result<PathBuf, String> {
        // sockaddr_un is only 104 bytes on macOS; its normal TMPDIR can be longer.
        let dir = std::fs::canonicalize("/tmp")
            .map_err(|e| e.to_string())?
            .join(format!("ta-agenda-{}", supervisor::user_id()));
        seat_store::private_dir(&dir)?;
        Ok(dir)
    }
    fn socket_path(store: &Store) -> Result<PathBuf, String> {
        let label = supervisor::label(store);
        let digest = label.rsplit('.').next().ok_or("invalid agenda label")?;
        Ok(socket_dir()?.join(format!("{}.sock", &digest[..32])))
    }
    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    pub(super) fn wake(store: &Store, job: Option<&str>) -> Result<(), String> {
        let target = socket_path(store)?;
        let nonce = seat_store::random_instance()?;
        let client_path = socket_dir()?.join(format!("{nonce}.reply"));
        let client = UnixDatagram::bind(&client_path).map_err(|e| e.to_string())?;
        let _cleanup = SocketFile(client_path);
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let request = serde_json::to_vec(&json!({"v":1,"kind":"wake","nonce":nonce,"job_id":job}))
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut bytes = [0; 4096];
        loop {
            match client.send_to(&request, &target) {
                Ok(_) => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    ()
                }
                Err(e) => return Err(e.to_string()),
            }
            // Bounded startup/exit-race retry, not a resident polling timer.
            match client.recv(&mut bytes) {
                Ok(n) => {
                    let value: Value =
                        serde_json::from_slice(&bytes[..n]).map_err(|e| e.to_string())?;
                    if value["nonce"] == nonce && value["job_id"] == json!(job) {
                        if value["ok"] == true {
                            return Ok(());
                        }
                        return Err(value["error"]
                            .as_str()
                            .unwrap_or("agenda rejected job")
                            .into());
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    ()
                }
                Err(e) => return Err(e.to_string()),
            }
            if Instant::now() >= deadline {
                return Err("agenda activation was not acknowledged".into());
            }
        }
    }
    pub(super) fn notify(store: &Store) -> Result<(), String> {
        let socket = UnixDatagram::unbound().map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        socket
            .send_to(
                b"{\"v\":1,\"kind\":\"wake\",\"job_id\":null}",
                socket_path(store)?,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub(super) fn run(store: Store) -> Result<(), String> {
        let _runner = store.lock("runner")?;
        let address = socket_path(&store)?;
        match std::fs::remove_file(&address) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        let socket = UnixDatagram::bind(&address).map_err(|e| e.to_string())?;
        let _cleanup = SocketFile(address.clone());
        let (completed_tx, completed_rx) = mpsc::channel();
        let mut active = HashSet::new();
        let mut blocked = HashSet::new();
        let mut idle_since = None;
        let mut buffer = [0; 4096];
        loop {
            while let Ok((id, result)) = completed_rx.try_recv() {
                active.remove(&id);
                if let Err(error) = result {
                    eprintln!("agenda job {id}: {error}");
                    blocked.insert(id);
                }
            }
            let mut pending = false;
            let mut next = None::<Duration>;
            for (id, entry) in store.entries()? {
                let mut job = match entry {
                    Ok(job) => job,
                    Err(error) => {
                        eprintln!("agenda job {id}: {error}");
                        continue;
                    }
                };
                if !matches!(job.status.as_str(), "active" | "running") {
                    continue;
                }
                if active.contains(&id) || blocked.contains(&id) {
                    pending = true;
                    continue;
                }
                if job.status == "running" {
                    // The old executor died after durable admission. M1's dedupe is
                    // bounded, so an old running occurrence must NEVER be re-pasted.
                    job = match store.recover_interrupted(&id) {
                        Ok(job) => job,
                        Err(error) => {
                            eprintln!("agenda recovery {id}: {error}");
                            blocked.insert(id);
                            pending = true;
                            continue;
                        }
                    };
                    if job.status != "active" {
                        continue;
                    }
                }
                pending = true;
                let wait = job
                    .due()?
                    .signed_duration_since(Utc::now())
                    .to_std()
                    .unwrap_or_default();
                if active.len() >= WORKERS {
                    continue;
                }
                if wait.is_zero() {
                    active.insert(id.clone());
                    let worker = store.clone();
                    let tx = completed_tx.clone();
                    let wake_address = address.clone();
                    std::thread::Builder::new()
                        .name("agenda-job".into())
                        .spawn(move || {
                            let result = std::panic::catch_unwind(|| worker.fire(&id))
                                .unwrap_or_else(|_| {
                                    Err("agenda execution panicked; outcome uncertain".into())
                                });
                            // Publish completion before the wake; avoids an is_finished race.
                            let _ = tx.send((id, result));
                            if let Ok(socket) = UnixDatagram::unbound() {
                                let _ = socket.send_to(b"{}", wake_address);
                            }
                        })
                        .map_err(|e| e.to_string())?;
                } else {
                    next = Some(next.map_or(wait, |old| old.min(wait)));
                }
            }
            if !pending && active.is_empty() {
                let began = *idle_since.get_or_insert_with(Instant::now);
                if began.elapsed() >= IDLE {
                    // Producers hold activation across durable publish + ACK. Do not
                    // wait on them here: receive their wake first, then reconsider exit.
                    if let Some(_activation) = store.try_activation_lock()? {
                        if store.entries()?.iter().all(|(_, j)| {
                            j.as_ref().map_or(true, |j| {
                                !matches!(j.status.as_str(), "active" | "running")
                            })
                        }) {
                            supervisor::clear_pending(&store)?;
                            supervisor::retire(&store)?;
                            return Ok(());
                        }
                    }
                    next = Some(Duration::from_millis(100));
                } else {
                    next = Some(IDLE.saturating_sub(began.elapsed()));
                }
            } else {
                idle_since = None;
            }
            socket
                .set_read_timeout(next.map(|d| d.max(Duration::from_millis(1))))
                .map_err(|e| e.to_string())?;
            match socket.recv_from(&mut buffer) {
                Ok((size, peer)) => {
                    let Ok(request) = serde_json::from_slice::<Value>(&buffer[..size]) else {
                        continue;
                    };
                    if request["v"] != 1 || request["kind"] != "wake" {
                        continue;
                    }
                    blocked.clear();
                    let acknowledged = request["job_id"]
                        .as_str()
                        .map_or(Ok(()), |id| store.acknowledge(id));
                    let response = match acknowledged {
                        Ok(()) => {
                            json!({"v":1,"ok":true,"nonce":request["nonce"],"job_id":request["job_id"],"pid":std::process::id()})
                        }
                        Err(error) => {
                            json!({"v":1,"ok":false,"nonce":request["nonce"],"job_id":request["job_id"],"error":error})
                        }
                    };
                    if let Some(path) = peer.as_pathname() {
                        let bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
                        let _ = socket.send_to(&bytes, path);
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    ()
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

pub fn run(workspace: &Path, team: &str) -> Result<(), String> {
    let store = Store::new(workspace, team)?;
    #[cfg(unix)]
    {
        unix::run(store)
    }
    #[cfg(not(unix))]
    {
        let _ = store;
        Err("agenda runner unsupported on this platform".into())
    }
}
pub(super) fn notify(store: &Store) -> Result<(), String> {
    #[cfg(unix)]
    {
        unix::notify(store)
    }
    #[cfg(not(unix))]
    {
        let _ = store;
        Err("agenda wake unsupported on this platform".into())
    }
}
pub(super) fn wake(store: &Store, job: Option<&str>) -> Result<(), String> {
    #[cfg(unix)]
    {
        unix::wake(store, job)
    }
    #[cfg(not(unix))]
    {
        let _ = (store, job);
        Err("agenda wake unsupported on this platform".into())
    }
}
