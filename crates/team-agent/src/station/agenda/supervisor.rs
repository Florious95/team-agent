//! OS-supervised activation. A detached child alone is not a delivery promise.
use super::{seat_store, Store};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
pub(super) fn user_id() -> u32 {
    // SAFETY: geteuid reads this process's credentials and takes no pointers.
    unsafe { libc::geteuid() }
}
#[cfg(unix)]
pub(super) fn runtime_dir() -> Result<PathBuf, String> {
    // Short, stable across sessions, and outside a workspace that may disappear.
    let dir = std::fs::canonicalize("/tmp")
        .map_err(|e| e.to_string())?
        .join(format!("ta-agenda-{}", user_id()));
    seat_store::private_dir(&dir)?;
    Ok(dir)
}
pub(super) fn activation_lock_path(store: &Store) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        Ok(runtime_dir()?.join(format!("{}.lock", label(store))))
    }
    #[cfg(not(unix))]
    {
        let dir = store.root.join("locks");
        seat_store::private_dir(&dir)?;
        Ok(dir.join("activation.lock"))
    }
}
pub(super) fn label(store: &Store) -> String {
    let digest = Sha256::digest(store.root.to_string_lossy().as_bytes());
    format!("team-agent.agenda.{digest:x}")
}
pub(super) fn mark_pending(store: &Store) -> Result<(), String> {
    seat_store::write_json(
        &store.root.join("pending.json"),
        &json!({"v":1,"kind":"agenda_activation","workspace":store.workspace,"team":store.team}),
        8192,
    )
}
pub(super) fn clear_pending(store: &Store) -> Result<(), String> {
    match std::fs::remove_file(store.root.join("pending.json")) {
        Ok(()) => seat_store::sync_directory(&store.root),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
fn checked(command: &mut Command) -> Result<(), String> {
    checked_output(command).map(|_| ())
}
fn checked_output(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command
        .output()
        .map_err(|e| format!("agenda activation unavailable: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "agenda activation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME unavailable for user supervisor".into())
}
fn publish(path: &std::path::Path, bytes: &[u8]) -> Result<bool, String> {
    use std::io::Write;
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    let parent = path.parent().ok_or("missing supervisor directory")?;
    seat_store::private_dir(parent)?;
    let temp = parent.join(format!(".agenda-{}.tmp", seat_store::random_instance()?));
    let result = (|| {
        let mut file = seat_store::open_file(&temp, true)?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
        seat_store::sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map(|()| true)
}

#[cfg(target_os = "macos")]
struct Registration {
    store: Store,
    plist: PathBuf,
}
#[cfg(target_os = "macos")]
fn agenda_labels(snapshot: &str) -> std::collections::BTreeSet<&str> {
    snapshot
        .lines()
        .filter_map(|line| line.split_whitespace().last())
        .filter(|label| {
            label
                .strip_prefix("team-agent.agenda.")
                .is_some_and(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
        .collect()
}
#[cfg(target_os = "macos")]
fn registration(expected_label: &str, snapshot: &str) -> Option<Registration> {
    // launchctl's loaded definition survives deletion of the source plist. Parse only
    // our exact argv contract and verify its scope hash; unfamiliar definitions stay intact.
    let field = |name: &str| {
        snapshot
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("{name} = ")))
    };
    let arguments = snapshot
        .split_once("arguments = {\n")?
        .1
        .lines()
        .map(str::trim)
        .take_while(|line| *line != "}")
        .collect::<Vec<_>>();
    if arguments.len() != 6
        || arguments[1] != "agenda-run"
        || arguments[2] != "--workspace"
        || arguments[4] != "--team"
        || field("working directory")? != arguments[3]
    {
        return None;
    }
    seat_store::component(arguments[5]).ok()?;
    let workspace = PathBuf::from(arguments[3]);
    let plist = PathBuf::from(field("path")?);
    if !workspace.is_absolute()
        || !plist.is_absolute()
        || !plist.ends_with(
            PathBuf::from("Library/LaunchAgents").join(format!("{expected_label}.plist")),
        )
    {
        return None;
    }
    let store = Store {
        root: workspace.join(".team/agenda").join(arguments[5]),
        workspace,
        team: arguments[5].into(),
    };
    (label(&store) == expected_label).then_some(Registration { store, plist })
}
#[cfg(target_os = "macos")]
fn missing_directory(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => !metadata.is_dir(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}
#[cfg(target_os = "macos")]
fn orphaned(registration: &Registration) -> bool {
    if missing_directory(&registration.store.workspace)
        || missing_directory(&registration.store.root)
    {
        return true;
    }
    // A missing plist alone does not invalidate a live, loaded timer. Preserve
    // pending/unknown work; only an idle registration is disposable in this case.
    matches!(std::fs::symlink_metadata(&registration.plist),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        && registration.store.entries().is_ok_and(|entries| {
            entries.iter().all(|(_, job)| {
                job.as_ref()
                    .is_ok_and(|job| matches!(job.status.as_str(), "done" | "cancelled" | "failed"))
            })
        })
}
#[cfg(target_os = "macos")]
fn unregister(domain: &str, name: &str, plist: &std::path::Path) -> Result<(), String> {
    // Remove the source first: self-bootout may terminate us before it returns.
    // A later activation can recover a loaded-only registration from launchctl.
    let removal = remove_registration(plist);
    let unload = checked(Command::new("launchctl").args(["bootout", &format!("{domain}/{name}")]));
    if let Err(error) = unload {
        // Another observer may already have removed it. Absence must be proved
        // by a successful domain query, never inferred from a refused command.
        let snapshot = checked_output(Command::new("launchctl").args(["print", domain]))?;
        if agenda_labels(&String::from_utf8_lossy(&snapshot)).contains(name) {
            return Err(error);
        }
    }
    removal
}
#[cfg(target_os = "macos")]
fn sweep(current: &Store, domain: &str, snapshot: &[u8]) {
    let snapshot = String::from_utf8_lossy(snapshot);
    let current_label = label(current);
    for name in agenda_labels(&snapshot) {
        if name == current_label {
            continue;
        }
        let attempt = (|| {
            let data = checked_output(
                Command::new("launchctl").args(["print", &format!("{domain}/{name}")]),
            )?;
            let Some(registration) = registration(name, &String::from_utf8_lossy(&data)) else {
                return Ok(());
            };
            if !orphaned(&registration) {
                return Ok(());
            }
            // Never wait while holding another scope's activation lock.
            let Some(_lock) = registration.store.try_activation_lock()? else {
                return Ok(());
            };
            if orphaned(&registration) {
                unregister(domain, name, &registration.plist)?;
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = attempt {
            eprintln!("agenda orphan sweep {name}: {error}");
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn activate(store: &Store) -> Result<(), String> {
    fn xml(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
    let label = label(store);
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let argv = [
        executable.to_string_lossy().into_owned(),
        "agenda-run".into(),
        "--workspace".into(),
        store.workspace.to_string_lossy().into_owned(),
        "--team".into(),
        store.team.clone(),
    ]
    .iter()
    .map(|s| format!("<string>{}</string>", xml(s)))
    .collect::<String>();
    let config = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{argv}</array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict><key>KeepAlive</key><dict><key>PathState</key><dict><key>{}</key><true/></dict></dict></dict></plist>\n",
        xml(&label), xml(&store.workspace.to_string_lossy()), xml(&std::env::var("PATH").unwrap_or_default()),
        xml(&store.root.join("pending.json").to_string_lossy()));
    let path = home()?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    let uid = user_id();
    let domain = format!("gui/{uid}");
    // A refused public manager must not overwrite a previously valid registration.
    let snapshot = checked_output(Command::new("launchctl").args(["print", &domain]))?;
    sweep(store, &domain, &snapshot);
    publish(&path, config.as_bytes())?;
    let service = format!("{domain}/{label}");
    if !Command::new("launchctl")
        .args(["print", &service])
        .output()
        .map_err(|e| e.to_string())?
        .status
        .success()
    {
        checked(
            Command::new("launchctl")
                .args(["bootstrap", &domain])
                .arg(&path),
        )?;
    }
    // Never use -k: killing an in-flight executor would introduce ambiguity.
    checked(Command::new("launchctl").args(["kickstart", &service]))
}

#[cfg(target_os = "linux")]
pub(super) fn activate(store: &Store) -> Result<(), String> {
    fn quote(s: &str) -> String {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('%', "%%")
        )
    }
    let label = label(store);
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = home()?.join(".config/systemd/user");
    let service = format!("[Unit]\nDescription=Team Agent scoped agenda\n[Service]\nType=simple\nExecStart=:{} agenda-run --workspace {} --team {}\nEnvironment={}\nRestart=on-failure\nRestartSec=1\n",
        quote(&executable.to_string_lossy()), quote(&store.workspace.to_string_lossy()), quote(&store.team),
        quote(&format!("PATH={}", std::env::var("PATH").unwrap_or_default())));
    let watcher = format!("[Unit]\nDescription=Team Agent pending jobs activation\n[Path]\nPathExists={}\nUnit={label}.service\n[Install]\nWantedBy=default.target\n", quote(&store.root.join("pending.json").to_string_lossy()));
    let changed_service = publish(&dir.join(format!("{label}.service")), service.as_bytes())?;
    let changed_path = publish(&dir.join(format!("{label}.path")), watcher.as_bytes())?;
    if changed_service || changed_path {
        checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
    }
    checked(Command::new("systemctl").args([
        "--user",
        "enable",
        "--now",
        &format!("{label}.path"),
    ]))?;
    checked(Command::new("systemctl").args(["--user", "start", &format!("{label}.service")]))
}

fn remove_registration(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => seat_store::sync_directory(path.parent().ok_or("missing registration parent")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Called under the activation lock after an empty-job rescan. Native unregister
/// may terminate this process; kernel lock release then admits the next producer.
#[cfg(target_os = "macos")]
pub(super) fn retire(store: &Store) -> Result<(), String> {
    let label = label(store);
    let domain = format!("gui/{}", user_id());
    let loaded =
        checked_output(Command::new("launchctl").args(["print", &format!("{domain}/{label}")]))
            .ok()
            .and_then(|snapshot| registration(&label, &String::from_utf8_lossy(&snapshot)));
    let plist = match loaded {
        Some(registration) => registration.plist,
        None => home()?
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist")),
    };
    unregister(&domain, &label, &plist)
}
#[cfg(target_os = "linux")]
pub(super) fn retire(store: &Store) -> Result<(), String> {
    let label = label(store);
    checked(Command::new("systemctl").args([
        "--user",
        "disable",
        "--now",
        &format!("{label}.path"),
    ]))?;
    let directory = home()?.join(".config/systemd/user");
    remove_registration(&directory.join(format!("{label}.path")))?;
    remove_registration(&directory.join(format!("{label}.service")))?;
    checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
    checked(Command::new("systemctl").args(["--user", "stop", &format!("{label}.service")]))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn retire(_store: &Store) -> Result<(), String> {
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn activate(_store: &Store) -> Result<(), String> {
    Err("reliable agenda activation is unsupported on this platform; job remains stored".into())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::super::tests::Fixture;
    use super::*;

    fn snapshot(store: &Store, plist: &std::path::Path) -> String {
        format!("path = {}\narguments = {{\n/candidate/team-agent\nagenda-run\n--workspace\n{}\n--team\n{}\n}}\nworking directory = {}\n",
            plist.display(), store.workspace.display(), store.team, store.workspace.display())
    }

    #[test]
    fn loaded_definition_is_owned_even_after_its_plist_disappears() {
        let fixture = Fixture::new();
        let store = fixture.store("alpha");
        let name = label(&store);
        let plist = store
            .workspace
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist"));
        let printed = snapshot(&store, &plist);
        let registration = registration(&name, &printed).expect("owned definition");
        assert!(orphaned(&registration), "the agenda root does not exist");
        assert!(super::registration("team-agent.agenda.wrong", &printed).is_none());
        assert!(
            super::registration(&name, &printed.replace("agenda-run", "unrelated-service"))
                .is_none()
        );
        assert!(super::registration(
            &name,
            &printed.replace("working directory = ", "unknown = ")
        )
        .is_none());
    }

    #[test]
    fn missing_plist_never_discards_live_or_unreadable_work() {
        let fixture = Fixture::new();
        let store = fixture.store("alpha");
        store.write(&fixture.job("sched_a")).unwrap();
        let name = label(&store);
        let plist = store
            .workspace
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist"));
        let registration = registration(&name, &snapshot(&store, &plist)).unwrap();
        assert!(!orphaned(&registration));
        std::fs::write(store.path("sched_a").unwrap(), b"{broken").unwrap();
        assert!(!orphaned(&registration));
        std::fs::remove_file(store.path("sched_a").unwrap()).unwrap();
        assert!(
            orphaned(&registration),
            "missing plist with no obligations is disposable"
        );
        seat_store::private_dir(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, b"fixture").unwrap();
        assert!(!orphaned(&registration));
        std::fs::remove_dir_all(&store.root).unwrap();
        assert!(
            orphaned(&registration),
            "a vanished scope is orphaned even if the plist survives"
        );
    }

    #[test]
    fn domain_scan_uses_only_complete_agenda_labels() {
        let name = format!("team-agent.agenda.{}", "a".repeat(64));
        let data =
            format!("0 78 {name}\n1 0 unrelated-service\nteam-agent.agenda.bad\n0 0 {name}\n");
        assert_eq!(
            agenda_labels(&data).into_iter().collect::<Vec<_>>(),
            vec![name.as_str()]
        );
    }
}
