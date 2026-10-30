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
    let output = command
        .output()
        .map_err(|e| format!("agenda activation unavailable: {e}"))?;
    if output.status.success() {
        Ok(())
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
    let config = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{argv}</array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict><key>KeepAlive</key><dict><key>PathState</key><dict><key>{}</key><true/></dict></dict><key>ThrottleInterval</key><integer>1</integer></dict></plist>\n",
        xml(&label), xml(&store.workspace.to_string_lossy()), xml(&std::env::var("PATH").unwrap_or_default()),
        xml(&store.root.join("pending.json").to_string_lossy()));
    let path = home()?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    publish(&path, config.as_bytes())?;
    let uid = user_id();
    let domain = format!("gui/{uid}");
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn activate(_store: &Store) -> Result<(), String> {
    Err("reliable agenda activation is unsupported on this platform; job remains stored".into())
}
