use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::contract::plan::EnvironmentDelta;
use crate::contract::types::require_absolute;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLimits {
    pub stdout: usize,
    pub stderr: usize,
    pub stdin: usize,
}

/// Remaining parent budget, never a new unbounded child timeout. No Debug payload dump.
pub struct CommandRequest {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub environment: EnvironmentDelta,
    pub stdin: Option<Vec<u8>>,
    pub budget: Duration,
    pub limits: OutputLimits,
    /// A captured read-command guard, not an instruction parsed from child output.
    /// Matching cancels only this owned child; it never initiates authentication.
    pub reject_stdout: Option<&'static [u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandEnd {
    Exited,
    NotStarted,
    TimedOut,
    OutputLimit,
    OutputRejected,
    IoError,
    Unsupported,
}

/// Exit status and capture completion are independent. Exited(0) is required for success.
/// An error after spawning does NOT prove that the requested action had no effects.
pub struct CommandReceipt {
    pub end: CommandEnd,
    pub exit_code: Option<i32>,
    pub child_pid: Option<u32>,
    pub child_reaped: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed: Duration,
}
impl CommandReceipt {
    pub fn success(&self) -> bool {
        self.end == CommandEnd::Exited && self.exit_code == Some(0)
    }
    pub fn may_have_executed(&self) -> bool {
        self.child_pid.is_some()
    }
}

pub trait CommandRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt;
}

/// Nonblocking pipes: blocked stdin or inherited output handles cannot cause a reader join.
/// Only the exact child created here may be killed. Unreaped handles remain owned/reported.
#[derive(Default)]
pub struct RealCommandRunner {
    pending_reap: Vec<Child>,
}

impl RealCommandRunner {
    pub fn pending_children(&self) -> Vec<u32> {
        self.pending_reap.iter().map(Child::id).collect()
    }

    pub fn reap_owned(&mut self) {
        self.pending_reap
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
    }
}

fn before_start(end: CommandEnd, started: Instant) -> CommandReceipt {
    CommandReceipt {
        end,
        exit_code: None,
        child_pid: None,
        child_reaped: true,
        stdout: Vec::new(),
        stderr: Vec::new(),
        elapsed: started.elapsed(),
    }
}

#[cfg(unix)]
fn nonblocking(fd: &impl std::os::fd::AsFd) -> std::io::Result<()> {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    let current = fcntl(fd, FcntlArg::F_GETFL).map_err(std::io::Error::from)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK),
    )
    .map(|_| ())
    .map_err(std::io::Error::from)
}

fn drain(reader: &mut impl Read, output: &mut Vec<u8>, limit: usize) -> Result<bool, CommandEnd> {
    let mut buffer = [0_u8; 8192];
    // Bound each turn too: a continuously writing child cannot starve deadline checks.
    for _ in 0..8 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                let keep = count.min(limit.saturating_sub(output.len()));
                output.extend_from_slice(&buffer[..keep]);
                if keep != count {
                    return Err(CommandEnd::OutputLimit);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(CommandEnd::IoError),
        }
    }
    Ok(false)
}

impl CommandRunner for RealCommandRunner {
    fn run(&mut self, request: &CommandRequest) -> CommandReceipt {
        let started = Instant::now();
        self.reap_owned();
        let Some(deadline) = started.checked_add(request.budget) else {
            return before_start(CommandEnd::NotStarted, started);
        };
        if request.budget.is_zero()
            || request.limits.stdout == 0
            || request.limits.stderr == 0
            || require_absolute(&request.executable, "command executable").is_err()
            || request
                .cwd
                .as_ref()
                .is_some_and(|cwd| require_absolute(cwd, "command cwd").is_err())
            || request.environment.validate().is_err()
            || request
                .arguments
                .iter()
                .any(|arg| arg.as_encoded_bytes().contains(&0))
            || request
                .stdin
                .as_ref()
                .is_some_and(|bytes| bytes.len() > request.limits.stdin)
            || request
                .reject_stdout
                .is_some_and(|pattern| pattern.is_empty() || pattern.len() > request.limits.stdout)
        {
            return before_start(CommandEnd::NotStarted, started);
        }
        #[cfg(not(unix))]
        {
            return before_start(CommandEnd::Unsupported, started);
        }
        #[cfg(unix)]
        {
            let mut command = Command::new(&request.executable);
            command
                .args(&request.arguments)
                .stdin(if request.stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(cwd) = &request.cwd {
                command.current_dir(cwd);
            }
            for key in &request.environment.remove {
                command.env_remove(key);
            }
            command.envs(&request.environment.set);
            let Ok(mut child) = command.spawn() else {
                return before_start(CommandEnd::NotStarted, started);
            };
            let mut result = CommandReceipt {
                end: CommandEnd::IoError,
                exit_code: None,
                child_pid: Some(child.id()),
                child_reaped: false,
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed: Duration::ZERO,
            };
            let mut input = child.stdin.take();
            let mut output = child.stdout.take();
            let mut errors = child.stderr.take();
            let mut setup_ok = true;
            if let Some(fd) = &input {
                setup_ok &= nonblocking(fd).is_ok();
            }
            if let Some(fd) = &output {
                setup_ok &= nonblocking(fd).is_ok();
            } else {
                setup_ok = false;
            }
            if let Some(fd) = &errors {
                setup_ok &= nonblocking(fd).is_ok();
            } else {
                setup_ok = false;
            }
            if setup_ok {
                let bytes = request.stdin.as_deref().unwrap_or_default();
                let mut written = 0;
                result.end = loop {
                    if Instant::now() >= deadline {
                        break CommandEnd::TimedOut;
                    }
                    if !result.child_reaped {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                result.exit_code = status.code();
                                result.child_reaped = true;
                            }
                            Ok(None) => {}
                            Err(_) => break CommandEnd::IoError,
                        }
                    }
                    if let Some(pipe) = &mut input {
                        if written == bytes.len() {
                            input = None;
                        } else {
                            let end = bytes.len().min(written.saturating_add(8192));
                            match pipe.write(&bytes[written..end]) {
                                Ok(0) => break CommandEnd::IoError,
                                Ok(count) => written += count,
                                Err(error)
                                    if error.kind() == std::io::ErrorKind::WouldBlock
                                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                                Err(_) => break CommandEnd::IoError,
                            }
                        }
                    }
                    if let Some(pipe) = &mut output {
                        let drained = drain(pipe, &mut result.stdout, request.limits.stdout);
                        if request.reject_stdout.is_some_and(|pattern| {
                            result
                                .stdout
                                .windows(pattern.len())
                                .any(|window| window == pattern)
                        }) {
                            break CommandEnd::OutputRejected;
                        }
                        match drained {
                            Ok(true) => output = None,
                            Ok(false) => {}
                            Err(end) => break end,
                        }
                    }
                    if let Some(pipe) = &mut errors {
                        match drain(pipe, &mut result.stderr, request.limits.stderr) {
                            Ok(true) => errors = None,
                            Ok(false) => {}
                            Err(end) => break end,
                        }
                    }
                    if result.child_reaped
                        && output.is_none()
                        && errors.is_none()
                        && input.is_none()
                    {
                        break CommandEnd::Exited;
                    }
                    std::thread::sleep(
                        Duration::from_millis(1)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                };
            }
            // Closing pipes is bounded even if an unrelated descendant inherited the writer.
            drop(input);
            drop(output);
            drop(errors);
            if !result.child_reaped {
                let _ = child.kill();
                match child.try_wait() {
                    Ok(Some(status)) => {
                        result.exit_code = status.code();
                        result.child_reaped = true;
                    }
                    _ => self.pending_reap.push(child),
                }
            }
            result.elapsed = started.elapsed();
            result
        }
    }
}
