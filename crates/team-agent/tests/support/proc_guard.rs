//! Test-owned lifelines. No host-wide kill or shared tmux-directory sweep.
// Included by both standalone integration tests and shared in-process fixtures.
#![allow(clippy::duplicate_mod)]
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// The watchdog joins the fixture group and keeps its marker alive even after a
// double-hop parent exits. Its timer/readers inherit that group too. Killing a
// group containing the watchdog itself cannot target a reused process group.
const GROUP_WATCHDOG: &str = r#"
trap '' INT HUP TERM;
group=$1; ttl=$3;
(sleep "$ttl"; kill -KILL -"$group") &
cat >/dev/null;
kill -KILL -"$group";
"#;

// Only descend into real directories under our private root (never symlinks).
// USR1 interrupts the shell's wait on the lifeline reader. The timer retains a
// final five-second cleanup bound, so a broken tmux command cannot hang forever.
const TMUX_WATCHDOG: &str = r#"
trap '' INT HUP TERM;
trap ':' USR1;
root=$1; tmux=$2; ttl=$3;
walk() {
    [ "$2" -gt 0 ] || return;
    for entry in "$1"/*; do
        [ -L "$entry" ] && continue;
        if [ -S "$entry" ]; then "$tmux" -S "$entry" kill-server >/dev/null 2>&1 || :;
        elif [ -d "$entry" ]; then walk "$entry" "$(($2 - 1))"; fi;
    done;
};
exec 3<&0;
(trap '' USR1; sleep "$ttl"; kill -USR1 -"$$"; sleep 5; kill -KILL -"$$") &
cat <&3 >/dev/null & reader=$!;
wait "$reader" || :;
walk "$root" 4;
rm -rf -- "$root";
kill -KILL -"$$";
"#;

#[cfg(unix)]
struct Watchdog {
    child: Child,
    lifeline: Option<ChildStdin>,
}

#[cfg(unix)]
impl Watchdog {
    fn spawn(script: &str, args: &[&std::ffi::OsStr], group: u32) -> std::io::Result<Self> {
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                &script.lines().map(str::trim).collect::<Vec<_>>().join(" "),
                "fixture-watchdog",
            ])
            .args(args)
            // These are OS cleanup primitives, not a provider launch or a SUT
            // probe. Do not inherit a test's intentionally failing ps/sleep shim.
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group as i32)
            .spawn()?;
        let lifeline = child.stdin.take();
        Ok(Self { child, lifeline })
    }
}

#[cfg(unix)]
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.lifeline.take();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
pub fn kill_marked_group(group: u32, marker: &Path) -> bool {
    if group <= 1 {
        return false;
    }
    let Ok(output) = Command::new("/bin/ps")
        .args(["-ww", "-axo", "pgid=,command="])
        .output()
    else {
        return false;
    };
    let marker = marker.to_string_lossy();
    let owns_group = String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let mut fields = line.trim_start().splitn(2, char::is_whitespace);
        fields.next().and_then(|field| field.parse::<u32>().ok()) == Some(group)
            && fields
                .next()
                .is_some_and(|command| command.contains(marker.as_ref()))
    });
    if owns_group {
        // A positive, test-owned PGID was matched to its unique marker.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(group as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    owns_group
}

#[cfg(unix)]
pub struct FixtureGroup {
    child: Child,
    watchdog: Option<Watchdog>,
    group: u32,
    marker: PathBuf,
}

#[cfg(unix)]
impl FixtureGroup {
    pub fn spawn(command: &mut Command, marker: &Path, ttl: Duration) -> Self {
        assert!(!ttl.is_zero() && ttl <= Duration::from_secs(120));
        let mut child = command
            .process_group(0)
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn owned fixture");
        let group = child.id();
        let group_text = group.to_string();
        let seconds = ttl.as_secs().max(1).to_string();
        let watchdog = Watchdog::spawn(
            GROUP_WATCHDOG,
            &[group_text.as_ref(), marker.as_os_str(), seconds.as_ref()],
            group,
        );
        let watchdog = match watchdog {
            Ok(watchdog) => watchdog,
            Err(error) => {
                // The unreaped direct child still pins this newly created PGID.
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(group as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                let _ = child.wait();
                panic!("spawn fixture lifeline: {error}");
            }
        };
        Self {
            child,
            watchdog: Some(watchdog),
            group,
            marker: marker.to_owned(),
        }
    }

    pub fn presence(marker: &Path, ttl: Duration) -> Self {
        Self::spawn(
            Command::new("/bin/sh")
                .args(["-c", "read _; exit 0"])
                .arg(marker)
                .current_dir(marker),
            marker,
            ttl,
        )
    }

    /// The inner process is reparented, but never escapes our owned group.
    pub fn detached(
        marker: &Path,
        script: &str,
        args: &[std::ffi::OsString],
        ttl: Duration,
    ) -> (Self, u32) {
        use std::io::BufRead;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", r#"/bin/sh -c 'script=$1; shift; /bin/sh -c "$script" "$0" "$@" >/dev/null 2>&1 & echo $!' "$0" "$@"; read _; exit 0"#])
            .arg(marker).arg(script).args(args).current_dir(marker)
            .stdout(Stdio::piped()).stderr(Stdio::null());
        let mut guard = Self::spawn(&mut command, marker, ttl);
        let mut line = String::new();
        std::io::BufReader::new(guard.child.stdout.take().expect("detached pid pipe"))
            .read_line(&mut line)
            .expect("read detached pid");
        let pid = line.trim().parse().expect("detached fixture pid");
        (guard, pid)
    }
}

#[cfg(unix)]
impl std::ops::Deref for FixtureGroup {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
#[cfg(unix)]
impl std::ops::DerefMut for FixtureGroup {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
#[cfg(unix)]
impl Drop for FixtureGroup {
    fn drop(&mut self) {
        kill_marked_group(self.group, &self.marker);
        // EOF is also a safe fallback if the process observation was unavailable.
        self.watchdog.take();
        self.child.stdin.take();
        let _ = self.child.wait();
    }
}

pub struct TmuxRoot {
    path: PathBuf,
    #[cfg(unix)]
    watchdog: Option<Watchdog>,
}

impl Default for TmuxRoot {
    fn default() -> Self {
        Self::new()
    }
}

impl TmuxRoot {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let path = base.join(format!(
                "tat-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                            .expect("private socket root");
                        let tmux = std::env::var_os("CONTRACT_TMUX")
                            .map(PathBuf::from)
                            .or_else(|| {
                                std::env::split_paths(&std::env::var_os("PATH")?)
                                    .map(|dir| dir.join("tmux"))
                                    .find(|path| path.is_file())
                            })
                            .unwrap_or_else(|| PathBuf::from("tmux"));
                        let watchdog = Watchdog::spawn(
                            TMUX_WATCHDOG,
                            &[path.as_os_str(), tmux.as_os_str(), "110".as_ref()],
                            0,
                        )
                        .expect("spawn private tmux reaper");
                        return Self {
                            path,
                            watchdog: Some(watchdog),
                        };
                    }
                    #[cfg(not(unix))]
                    return Self { path };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private tmux root: {error}"),
            }
        }
        panic!("private tmux roots occupied");
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn configure(&self, command: &mut Command) {
        command.env("TMUX_TMPDIR", &self.path);
    }
    pub fn close(&mut self) {
        #[cfg(unix)]
        self.watchdog.take();
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for TmuxRoot {
    fn drop(&mut self) {
        self.close();
    }
}
