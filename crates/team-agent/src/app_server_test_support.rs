use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct FakeAppServerScript {
    pub user_agent: Option<String>,
    pub thread_id: String,
    pub session_id: String,
    pub cwd: String,
    pub resume_error: Option<String>,
    pub turn_error: Option<String>,
    pub turn_status: String,
}

impl FakeAppServerScript {
    pub(crate) fn happy(thread_id: &str, session_id: &str, cwd: &str) -> Self {
        Self {
            user_agent: Some("codex-appserver-team-agent-test/0.139.0".to_string()),
            thread_id: thread_id.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            resume_error: None,
            turn_error: None,
            turn_status: "inProgress".to_string(),
        }
    }
}

pub(crate) struct FakeAppServer {
    endpoint: String,
    path: PathBuf,
    received_turns: Arc<Mutex<Vec<Value>>>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeAppServer {
    pub(crate) fn start(tag: &str, script: FakeAppServerScript) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/taas-{}-{id}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let received_turns = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));
        let thread_turns = Arc::clone(&received_turns);
        let thread_running = Arc::clone(&running);
        let thread_path = path.clone();
        let handle = std::thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = handle_connection(&mut stream, &script, &thread_turns);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
            let _ = std::fs::remove_file(thread_path);
        });
        Self {
            endpoint: format!("unix://{}", path.display()),
            path,
            received_turns,
            running,
            handle: Some(handle),
        }
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn path(&self) -> &PathBuf {
        &self.path
    }

    pub(crate) fn received_turns(&self) -> Vec<Value> {
        self.received_turns.lock().unwrap().clone()
    }
}

impl Drop for FakeAppServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.path);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn handle_connection(
    stream: &mut UnixStream,
    script: &FakeAppServerScript,
    turns: &Arc<Mutex<Vec<Value>>>,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    read_http_upgrade(stream)?;
    stream.write_all(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: test\r\n\r\n",
    )?;
    while let Some(frame) = read_ws_text(stream)? {
        let value: Value = match serde_json::from_str(&frame) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => {
                let mut result = serde_json::Map::new();
                result.insert("codexHome".to_string(), json!("/tmp/codex-home"));
                result.insert("platformFamily".to_string(), json!("unix"));
                result.insert("platformOs".to_string(), json!("macos"));
                if let Some(user_agent) = &script.user_agent {
                    result.insert("userAgent".to_string(), json!(user_agent));
                }
                write_ws_text(
                    stream,
                    &json!({"id": id, "result": Value::Object(result)}).to_string(),
                )?;
            }
            "initialized" => {}
            "thread/resume" => {
                if let Some(message) = &script.resume_error {
                    write_ws_text(
                        stream,
                        &json!({"id": id, "error": {"code": -32600, "message": message}})
                            .to_string(),
                    )?;
                } else {
                    write_ws_text(
                        stream,
                        &json!({
                            "id": id,
                            "result": {
                                "cwd": script.cwd,
                                "thread": {
                                    "id": script.thread_id,
                                    "sessionId": script.session_id,
                                    "cwd": script.cwd,
                                    "ephemeral": false
                                }
                            }
                        })
                        .to_string(),
                    )?;
                }
            }
            "turn/start" => {
                turns.lock().unwrap().push(value.clone());
                if let Some(message) = &script.turn_error {
                    write_ws_text(
                        stream,
                        &json!({"id": id, "error": {"code": -32600, "message": message}})
                            .to_string(),
                    )?;
                } else {
                    write_ws_text(
                        stream,
                        &json!({
                            "id": id,
                            "result": {
                                "turn": {"id": "turn-test", "status": script.turn_status}
                            }
                        })
                        .to_string(),
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn read_http_upgrade(stream: &mut UnixStream) -> std::io::Result<()> {
    let mut data = Vec::new();
    let mut buf = [0u8; 1];
    while !data.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut buf)?;
        data.push(buf[0]);
    }
    Ok(())
}

fn read_ws_text(stream: &mut UnixStream) -> std::io::Result<Option<String>> {
    let mut header = [0u8; 2];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    }
    let opcode = header[0] & 0x0f;
    if opcode == 0x8 {
        return Ok(None);
    }
    let masked = header[1] & 0x80 != 0;
    let mut len = u64::from(header[1] & 0x7f);
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        len = u64::from(u16::from_be_bytes(ext));
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; usize::try_from(len).unwrap_or(0)];
    stream.read_exact(&mut payload)?;
    if masked {
        for (idx, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[idx % 4];
        }
    }
    Ok(Some(String::from_utf8_lossy(&payload).to_string()))
}

fn write_ws_text(stream: &mut UnixStream, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}
