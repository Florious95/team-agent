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

/// A public command exactly as the user's shell runs it: the first executable
/// `name` on PATH. `entry` is that PATH entry as found (a user wrapper or a
/// vendor symlink stays itself); it is never canonicalized into an inner image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicCommand {
    pub name: String,
    /// Absolute PATH entries only, in order. This exact value is exported to the
    /// native process, so the shell's own lookup selects `entry` again.
    pub search_path: OsString,
    pub entry: PathBuf,
}

impl PublicCommand {
    /// Relative and empty PATH entries are dropped: the shell would resolve them
    /// against whatever the cwd is, so they are neither searched nor exported.
    pub fn resolve(name: &str, path: &std::ffi::OsStr) -> Result<Self, super::HostError> {
        use super::{HostError, HostErrorKind};
        if !crate::contract::types::valid_command_name(name) {
            return Err(HostError::new("public command name", HostErrorKind::Invalid));
        }
        let directories: Vec<PathBuf> = std::env::split_paths(path)
            .filter(|directory| require_absolute(directory, "PATH entry").is_ok())
            .collect();
        if directories.is_empty() {
            return Err(HostError::new("PATH entries", HostErrorKind::Invalid));
        }
        let search_path = std::env::join_paths(directories)
            .map_err(|_| HostError::new("PATH entries", HostErrorKind::Invalid))?;
        let entry = path_matches(name, &search_path).into_iter().next().ok_or_else(|| {
            HostError::new("public command not on PATH", HostErrorKind::Io(std::io::ErrorKind::NotFound))
        })?;
        Ok(Self { name: name.into(), search_path, entry })
    }

    /// Every executable match in PATH order; the first is `entry`. A wrapper
    /// conventionally execs a later match, so providers find the installation a
    /// route ends in from this list, never from a guessed install directory.
    pub fn route(&self) -> Vec<PathBuf> {
        path_matches(&self.name, &self.search_path)
    }

    /// The native image a route execs into: the first match whose resolved file
    /// is not a `#!` script. A user wrapper conventionally execs a later match;
    /// nothing outside the user's own PATH is searched or assumed.
    pub fn native_image(&self) -> Result<PathBuf, super::HostError> {
        use super::{HostError, HostErrorKind};
        for candidate in self.route() {
            let image = candidate.canonicalize().map_err(|error| HostError::io("public command route", error))?;
            let mut magic = Vec::with_capacity(2);
            std::fs::File::open(&image).and_then(|file| file.take(2).read_to_end(&mut magic))
                .map_err(|error| HostError::io("public command route image", error))?;
            if magic != b"#!" {
                return Ok(image);
            }
        }
        Err(HostError::new("native image not on PATH after its wrappers", HostErrorKind::Unsupported))
    }

    /// Like [`Self::native_image`], but a route that first reaches a vendor
    /// `#!<abs>/env <interpreter>` launcher script (an npm install) execs into
    /// the same PATH's native `interpreter`: `env` re-resolves it through the
    /// PATH this route exports. Any other `#!` script is a user wrapper.
    pub fn env_launcher_image(&self, interpreter: &str) -> Result<PathBuf, super::HostError> {
        use super::{HostError, HostErrorKind};
        for candidate in self.route() {
            let image = candidate.canonicalize().map_err(|error| HostError::io("public command route", error))?;
            let mut head = Vec::with_capacity(256);
            std::fs::File::open(&image).and_then(|file| file.take(256).read_to_end(&mut head))
                .map_err(|error| HostError::io("public command route image", error))?;
            if !head.starts_with(b"#!") {
                return Ok(image);
            }
            if env_shebang(&head, interpreter) {
                return Self::resolve(interpreter, &self.search_path)?.native_image();
            }
        }
        Err(HostError::new("native image not on PATH after its wrappers", HostErrorKind::Unsupported))
    }

    /// The pane entrypoint for this command, fencing the bytes PATH selected.
    pub fn launch_entrypoint(&self, sha256: crate::contract::types::Digest) -> crate::contract::plan::LaunchEntrypoint {
        crate::contract::plan::LaunchEntrypoint {
            command: self.name.clone(), search_path: self.search_path.clone(), executable: self.entry.clone(), sha256,
        }
    }
}

/// Exactly `#!<abs>/env <interpreter>`, no options: only then does `env`
/// look the interpreter up on PATH rather than run a fixed one.
fn env_shebang(head: &[u8], interpreter: &str) -> bool {
    let line = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let Some(line) = std::str::from_utf8(line).ok().and_then(|line| line.trim_end().strip_prefix("#!")) else { return false };
    let mut words = line.split_whitespace();
    matches!((words.next(), words.next(), words.next()), (Some(env), Some(name), None)
        if name == interpreter && env.starts_with('/') && std::path::Path::new(env).file_name().is_some_and(|file| file == "env"))
}

fn path_matches(name: &str, search_path: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .filter(|candidate| is_executable_file(candidate))
        .collect()
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &std::path::Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn public_command_keeps_the_path_entry_and_finds_the_image_after_wrappers() {
        let root = std::env::temp_dir().join(format!("ta-public-command-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (wrappers, vendor, image) = (root.join("wrappers"), root.join("vendor/bin"), root.join("vendor/lib/tool-native"));
        executable(&wrappers.join("tool"), b"#!/bin/sh\nexec tool \"$@\"\n");
        executable(&image, b"\x7fELF controlled native image");
        std::fs::create_dir_all(&vendor).unwrap();
        std::os::unix::fs::symlink(&image, vendor.join("tool")).unwrap();
        std::fs::write(root.join("not-executable"), b"x").unwrap();
        let path = std::env::join_paths([std::path::Path::new("relative/dir"), root.as_path(), wrappers.as_path(), vendor.as_path()]).unwrap();
        let command = PublicCommand::resolve("tool", &path).unwrap();
        assert_eq!(command.entry, wrappers.join("tool"), "the wrapper stays the public entry");
        assert_eq!(command.search_path, std::env::join_paths([&root, &wrappers, &vendor]).unwrap(), "relative PATH entries are dropped");
        assert_eq!(command.route(), [wrappers.join("tool"), vendor.join("tool")]);
        assert_eq!(command.native_image().unwrap(), image.canonicalize().unwrap());
        // Without a wrapper the vendor symlink itself is the entry; it is not canonicalized.
        let direct = PublicCommand::resolve("tool", vendor.as_os_str()).unwrap();
        assert_eq!(direct.entry, vendor.join("tool"));
        assert_eq!(direct.native_image().unwrap(), image.canonicalize().unwrap());
        // A wrapper whose native image is not on PATH is refused, never guessed.
        assert!(PublicCommand::resolve("tool", wrappers.as_os_str()).unwrap().native_image().is_err());
        for bad in ["", ".", "..", "/bin/tool", "dir/tool", "tool name"] {
            assert!(PublicCommand::resolve(bad, &path).is_err(), "{bad:?}");
        }
        assert!(PublicCommand::resolve("not-executable", &path).is_err());
        assert!(PublicCommand::resolve("tool", std::ffi::OsStr::new("relative:also/relative")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
