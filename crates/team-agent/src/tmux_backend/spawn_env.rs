//! Keep inherited values out of the tmux protocol and pane_start_command. The
//! pane reads a mode-0600 function definition and unlinks it before changing
//! cwd or launching anything. No secret is passed as a shell/utility argument.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum Invocation<'a> {
    Exec,
    Worker(&'a str),
    Leader(&'a str),
}

struct PrivateFile(PathBuf);
impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn private_file() -> std::io::Result<(PrivateFile, std::fs::File)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
        // The command remains visible after unlink. Other accounts must not
        // be able to recreate its path if someone replays the tmux command.
        let uid = unsafe { libc::geteuid() };
        let root = Path::new("/tmp").join(format!("team-agent-spawn-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(&root)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::other("unsafe tmux launch directory"));
        }
        let nonce = crate::station::seat_store::random_instance().map_err(std::io::Error::other)?;
        let path = root.join(nonce);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok((PrivateFile(path), file))
    }
    #[cfg(not(unix))]
    Err(std::io::Error::other(
        "private tmux launch environment requires Unix",
    ))
}

fn identifier(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|c| c == b'_' || c.is_ascii_alphabetic())
        && bytes.all(|c| c == b'_' || c.is_ascii_alphanumeric())
}

pub(super) fn with_command<T>(
    kind: Invocation<'_>,
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    unset: &[String],
    spawn: impl FnOnce(&str) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    if unset.iter().any(|key| !identifier(key)) {
        return Err(std::io::Error::other("invalid tmux environment key").into());
    }
    let worker = !matches!(kind, Invocation::Leader(_));
    let mut values = env
        .iter()
        .filter(|(key, _)| {
            !unset.contains(key)
                && !(worker && key.starts_with(crate::layout::worker_env::CALLER_CONTEXT_PREFIX))
        })
        .peekable();
    let mut public = BTreeMap::new();
    if worker {
        if let Some(provider) = caller_provider_for_invocation(env) {
            public.insert(
                crate::layout::worker_env::CALLER_PROVIDER_ENV.into(),
                provider.into(),
            );
        }
    }
    // The public provider identity is deliberately expanded in the target pane.
    // All other assignments apply to the provider only, never the inert tail.
    let file = if values.peek().is_some() {
        let (guard, mut f) = private_file()?;
        let (open, close) = if matches!(kind, Invocation::Exec) {
            ("{", "}")
        } else {
            ("(", ")")
        };
        writeln!(f, "_ta_exec() {open}")?;
        for (key, value) in values {
            if !identifier(key) || value.contains('\0') {
                return Err(std::io::Error::other("invalid tmux environment entry").into());
            }
            write!(f, "{key}={} ", shell_quote(value))?;
        }
        writeln!(f, "exec \"$@\"\n{close}")?;
        Some(guard)
    } else {
        None
    };

    let dispatch = if file.is_some() { "_ta_exec" } else { "exec" };
    let mut args = Vec::new();
    let wrapped = if file.is_some() && !matches!(kind, Invocation::Exec) {
        args.push(dispatch.to_string());
        args.extend_from_slice(argv);
        &args
    } else {
        argv
    };
    let mut cmd = match kind {
        Invocation::Exec => shell_command_with_exec(argv, cwd, &public, unset, dispatch),
        Invocation::Worker(label) => {
            worker_shell_wrapper_command(wrapped, cwd, &public, unset, label)
        }
        Invocation::Leader(label) => {
            leader_shell_wrapper_command(wrapped, cwd, &public, unset, label)
        }
    };
    if let Some(file) = &file {
        let path = shell_quote(&file.0.to_string_lossy());
        cmd = format!(". {path} && /bin/rm -f {path} && {cmd}");
    }
    let result = spawn(&cmd)?;
    if let Some(file) = &file {
        // tmux can acknowledge creation before the pane shell has run. Never
        // unlink early, nor leave an unread credential file after returning.
        let deadline = Instant::now() + Duration::from_secs(5);
        while file.0.try_exists()? {
            if Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "tmux pane did not consume its launch environment",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn small_environment_is_private_and_literal_too() {
        let value = "private ' quoted\n$HOME `literal`";
        let env = BTreeMap::from([
            ("TEST_PRIVATE".into(), value.into()),
            ("TEST_UNSET".into(), "ignored".into()),
        ]);
        let argv = [
            "/bin/sh".into(),
            "-c".into(),
            "test -z \"${TEST_UNSET+x}\" && printf %s \"$TEST_PRIVATE\"".into(),
        ];
        with_command(
            Invocation::Exec,
            &argv,
            Path::new("/"),
            &env,
            &["TEST_UNSET".into()],
            |cmd| {
                assert!(!cmd.contains("private ' quoted"));
                let path = Path::new(cmd.split_whitespace().nth(1).unwrap());
                assert_eq!(std::fs::metadata(path)?.mode() & 0o777, 0o600);
                let output = std::process::Command::new("/bin/sh")
                    .args(["-c", cmd])
                    .env("TEST_UNSET", "stale")
                    .output()?;
                assert!(output.status.success());
                assert_eq!(output.stdout, value.as_bytes());
                assert!(!path.exists());
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn refused_spawn_removes_unconsumed_environment() {
        let mut path = PathBuf::new();
        let out = with_command(
            Invocation::Exec,
            &[],
            Path::new("/"),
            &BTreeMap::from([("X".into(), "x".into())]),
            &[],
            |cmd| {
                path = PathBuf::from(cmd.split_whitespace().nth(1).unwrap());
                assert!(path.is_file());
                Err::<(), _>(std::io::Error::other("spawn refused").into())
            },
        );
        assert!(out.is_err());
        assert!(!path.exists());
    }
}
