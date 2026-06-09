use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(900);

thread_local! {
    static PROBE_TIMEOUT: RefCell<Option<ProbeTimeout>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub(crate) struct ProbeTimeout {
    pub(crate) probe: &'static str,
    pub(crate) pid: Option<u32>,
    pub(crate) timeout_ms: u64,
}

#[derive(Debug)]
pub(crate) struct BoundedCommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

pub(crate) fn clear_probe_timeout() {
    PROBE_TIMEOUT.with(|timeout| *timeout.borrow_mut() = None);
}

pub(crate) fn probe_timed_out() -> bool {
    PROBE_TIMEOUT.with(|timeout| timeout.borrow().is_some())
}

pub(crate) fn probe_timeout() -> Option<ProbeTimeout> {
    PROBE_TIMEOUT.with(|timeout| timeout.borrow().clone())
}

pub(crate) fn bounded_command_output_with_probe(
    command: &mut Command,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    bounded_command_output_with_timeout(command, DEFAULT_TIMEOUT, probe, pid)
}

fn bounded_command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    let stdout_path = temp_output_path("stdout");
    let stdout_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&stdout_path)?;
    let child = command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_bounded_child(child, stdout_file, stdout_path, timeout, probe, pid)
}

fn wait_for_bounded_child(
    mut child: std::process::Child,
    stdout_file: std::fs::File,
    stdout_path: std::path::PathBuf,
    timeout: Duration,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            drop(stdout_file);
            let stdout = read_and_remove(&stdout_path);
            return Ok(BoundedCommandOutput { status, stdout });
        }
        if start.elapsed() >= timeout {
            PROBE_TIMEOUT.with(|current| {
                let mut current = current.borrow_mut();
                if current.is_none() {
                    *current = Some(ProbeTimeout {
                        probe,
                        pid,
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
            });
            let _ = child.kill();
            let status = child.wait()?;
            drop(stdout_file);
            let stdout = read_and_remove(&stdout_path);
            return Ok(BoundedCommandOutput { status, stdout });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn temp_output_path(kind: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "team-agent-os-probe-{}-{nanos}.{kind}",
        std::process::id()
    ))
}

fn read_and_remove(path: &std::path::Path) -> Vec<u8> {
    let mut stdout = Vec::new();
    if let Ok(mut file) = std::fs::File::open(path) {
        let _ = file.read_to_end(&mut stdout);
    }
    let _ = std::fs::remove_file(path);
    stdout
}
