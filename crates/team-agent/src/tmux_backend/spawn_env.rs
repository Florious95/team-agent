//! Keep inherited values out of the tmux protocol and pane_start_command. The
//! pane sources a private one-shot file inside the provider's execution scope.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum Invocation<'a> {
    Exec,
    Worker(&'a str),
    Leader(&'a str),
}

struct PrivateFile(PathBuf, bool);
impl Drop for PrivateFile {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

// Expire abandoned one-shot files, not pending slow logins or unrelated paths.
// Sweep off the transport path and coalesce concurrent starts in one process.
#[cfg(unix)]
fn sweep(root: PathBuf) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    if ACTIVE.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::thread::Builder::new()
        .name("launch-env-gc".into())
        .spawn(move || {
            reap_expired(&root);
            ACTIVE.store(false, Ordering::Relaxed);
        })
        .is_err()
    {
        ACTIVE.store(false, Ordering::Relaxed);
    }
}

#[cfg(unix)]
fn reap_expired(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            continue;
        }
        if std::fs::symlink_metadata(entry.path()).is_ok_and(|m| {
            m.is_file()
                && m.modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age >= Duration::from_secs(3600))
        }) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn private_file() -> std::io::Result<(PrivateFile, std::fs::File)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
        // The command remains visible after unlink. Other accounts must not
        // be able to recreate its path if someone replays the tmux command.
        let uid = unsafe { libc::geteuid() };
        let temp = std::env::temp_dir().canonicalize()?;
        // The pane sources this absolute path later. A safe leaf alone isn't
        // enough if another account can replace one of its parent directories.
        for ancestor in temp.ancestors() {
            let meta = std::fs::symlink_metadata(ancestor)?;
            if !meta.is_dir()
                || (meta.uid() != uid && meta.uid() != 0)
                || (meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
            {
                return Err(std::io::Error::other("unsafe tmux temporary directory"));
            }
        }
        if temp.to_str().is_none() {
            return Err(std::io::Error::other("tmux temporary path is not UTF-8"));
        }
        let root = temp.join(format!("team-agent-spawn-{uid}"));
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
        sweep(root);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok((PrivateFile(path, false), file))
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
    let mut file = if values.peek().is_some() {
        let (guard, mut f) = private_file()?;
        // The shell opens the sourced file first; unlink before exporting any
        // values, using POSIX's utility search path rather than a fixed rm path.
        writeln!(
            f,
            "command -p rm -f {} || return 1",
            shell_quote(&guard.0.to_string_lossy())
        )?;
        for (key, value) in values {
            if !identifier(key) || value.contains('\0') {
                return Err(std::io::Error::other("invalid tmux environment entry").into());
            }
            writeln!(f, "export {key}={}", shell_quote(value))?;
        }
        Some(guard)
    } else {
        None
    };
    let path = file.as_ref().map(|f| f.0.as_path());
    // Wrappers keep an inert parent shell after the provider exits. Source the
    // file in its child only, so the tail cannot inherit the provider environment.
    let mut args = Vec::new();
    let wrapped = match (kind, path) {
        (Invocation::Worker(_) | Invocation::Leader(_), Some(path)) => {
            args.extend([
                "sh".into(),
                "-c".into(),
                ". \"$0\" && exec \"$@\"".into(),
                path.to_string_lossy().into_owned(),
            ]);
            args.extend_from_slice(argv);
            &args
        }
        _ => argv,
    };
    let cmd = match kind {
        Invocation::Exec => shell_command_with_env_file(argv, cwd, &public, unset, path),
        Invocation::Worker(label) => {
            worker_shell_wrapper_command(wrapped, cwd, &public, unset, label)
        }
        Invocation::Leader(label) => {
            leader_shell_wrapper_command(wrapped, cwd, &public, unset, label)
        }
    };
    let result = spawn(&cmd)?;
    // Acknowledging pane creation does not imply the login shell is running.
    // Hand cleanup to the pane on success; pre-spawn errors retain the RAII guard.
    if let Some(f) = file.as_mut() {
        f.1 = true;
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    // Extract only the generated nonce; the shell-quoted temp path may contain spaces.
    fn launch_file(cmd: &str) -> PathBuf {
        let dir = format!("team-agent-spawn-{}", unsafe { libc::geteuid() });
        let (_, suffix) = cmd.split_once(&format!("{dir}/")).unwrap();
        let nonce = &suffix[..32];
        assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()));
        std::env::temp_dir().join(dir).join(nonce)
    }

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
                let path = launch_file(cmd);
                assert_eq!(std::fs::metadata(&path)?.mode() & 0o777, 0o600);
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
                path = launch_file(cmd);
                assert!(path.is_file());
                Err::<(), _>(std::io::Error::other("spawn refused").into())
            },
        );
        assert!(out.is_err());
        assert!(!path.exists());
    }
}
