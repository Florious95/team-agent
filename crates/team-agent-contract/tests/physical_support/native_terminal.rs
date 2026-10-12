//! Owned native TTY fixture shared by the two real tmux test targets.
//! Not a provider, subscription, or genuine MCP client.
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use team_agent_contract::contract::hooks::{PlanHook, ProviderHooks};
use team_agent_contract::contract::plan::{CarrierRef, CarrierUse, LaunchPlan, ResolvedLaunch};
use team_agent_contract::contract::types::{ContractError, Digest, HookBinding};
use team_agent_contract::host::process::fingerprint_file;
use team_agent_contract::host::{digest, digest_hex};

pub fn copy_executable(root: &Path) -> (PathBuf, Digest) {
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let executable = root.join("native fixture '中文'");
    std::fs::copy(&candidate, &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        std::fs::metadata(&executable).unwrap().uid(),
        nix::unistd::geteuid().as_raw()
    );
    let native_hash = fingerprint_file(&executable, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap();
    assert_eq!(
        native_hash,
        fingerprint_file(&candidate, 512 * 1024 * 1024, Duration::from_secs(5)).unwrap()
    );
    (executable, native_hash)
}

pub struct NativeTerminal;
pub static PLAN: NativeTerminal = NativeTerminal;
impl PlanHook for NativeTerminal {
    fn plan(&self, launch: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let mut plan = super::HOOK.plan(launch)?;
        // Preserve the common fixture's identity/environment policy. Libtest's
        // exact ignored helper is the sole selectable test; the other positional
        // filters carry literal controlled operands after the -- boundary.
        plan.arguments = vec![
            "--exact".into(),
            "native_terminal_fixture".into(),
            "--ignored".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
            "--".into(),
            launch.model().unwrap_or("single").into(),
            launch.request().paths.runtime_root.join("native-events.jsonl").into_os_string(),
            launch.request().prompt.clone().unwrap_or_default().into(),
            "fixture-server".into(),
        ];
        if launch.model().is_some() {
            plan.carriers.model = CarrierUse::Specified(CarrierRef::Arguments(vec![6]));
        }
        plan.carriers.prompt = CarrierUse::Specified(CarrierRef::Arguments(vec![8]));
        plan.carriers.mcp = CarrierUse::Specified(CarrierRef::Arguments(vec![9]));
        Ok(plan)
    }
}

pub fn hooks() -> ProviderHooks<'static> {
    let mut hooks = super::hooks();
    hooks.plan = HookBinding::Bound(&PLAN);
    hooks
}

// Invoked by each test target's fixed, top-level ignored helper. Keep the raw
// byte parser identical to the host_native fixture that passed real macOS CI.
pub fn run() {
    use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg, Termios};

    struct RawTerminal(Termios);
    impl Drop for RawTerminal {
        fn drop(&mut self) {
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.0);
        }
    }
    fn event(log: &mut std::fs::File, value: Value) {
        serde_json::to_writer(&mut *log, &value).unwrap();
        log.write_all(b"\n").unwrap();
        log.flush().unwrap();
        log.sync_all().unwrap();
    }
    fn surface(name: &str, message: &str) {
        let mut output = std::io::stdout().lock();
        write!(output, "\x1b[2J\x1b[H{name}|{message}\r\n").unwrap();
        output.flush().unwrap();
    }

    // These are this fixture's own synthetic launch operands, not an inspection
    // of another process's command line or environment.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(args.len(), 10);
    assert_eq!(args[1], "native_terminal_fixture");
    assert_eq!(args[5], "--");
    let mode = args[6].to_str().unwrap();
    assert!(matches!(
        mode,
        "single" | "confirm" | "retry" | "queue" | "literal-control"
    ));
    let mut log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[7])
        .unwrap();
    let input = std::io::stdin();
    let saved = tcgetattr(&input).unwrap();
    let mut raw = saved.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&input, SetArg::TCSANOW, &raw).unwrap();
    let _restore = RawTerminal(saved);
    {
        let mut output = std::io::stdout().lock();
        output.write_all(b"\x1b[?2004h").unwrap();
        output.flush().unwrap();
    }
    event(
        &mut log,
        json!({
            "kind": "started", "pid": std::process::id(), "cwd": std::env::current_dir().unwrap(),
            "mode": mode, "prompt": args[8].to_str().unwrap(), "server": args[9].to_str().unwrap(),
            "fixture_env": std::env::var("CONTRACT_FIXTURE_ENV").ok(),
            "old_scope_present": std::env::var_os("CONTRACT_OLD_SCOPE").is_some(),
            "inherited_team_identity_present": std::env::vars_os()
                .any(|(key, _)| key.as_encoded_bytes().starts_with(b"TEAM_AGENT_"))
        }),
    );
    surface("READY", "-");
    let mut input = input.lock();
    let mut chunk = [0; 4096];
    let mut pending = Vec::new();
    let mut payload = Vec::new();
    let mut in_paste = false;
    let mut message = String::from("-");
    let mut keys = 0;
    const BEGIN: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    loop {
        let count = input.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..count]);
        while !pending.is_empty() {
            if in_paste {
                let Some(end) = pending.windows(END.len()).position(|part| part == END) else {
                    // Preserve a possibly split terminator, just like the former
                    // controlled fixture; never process paste newlines as keys.
                    let consumed = pending.len().saturating_sub(END.len() - 1);
                    payload.extend(pending.drain(..consumed));
                    break;
                };
                payload.extend_from_slice(&pending[..end]);
                drop(pending.drain(..end + END.len()));
                in_paste = false;
                message = "-".into();
                for tail in std::str::from_utf8(&payload)
                    .unwrap()
                    .split("[team-agent-token:")
                    .skip(1)
                {
                    if let Some((id, _)) = tail.split_once(']').filter(|(id, _)| {
                        !id.is_empty()
                            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    }) {
                        message = id.into();
                    }
                }
                event(
                    &mut log,
                    json!({"kind": "paste", "payload": payload,
                        "payload_sha256": digest_hex(digest(&payload)), "payload_bytes": payload.len(), "message": message}),
                );
                surface("PASTED", &message);
                continue;
            }
            if pending.starts_with(BEGIN) {
                drop(pending.drain(..BEGIN.len()));
                payload.clear();
                in_paste = true;
                keys = 0;
                continue;
            }
            if BEGIN.starts_with(&pending) {
                break;
            }
            let byte = pending.remove(0);
            if byte == b'\r' || byte == b'\n' {
                keys += 1;
                event(
                    &mut log,
                    json!({"kind": "key", "key": if byte == b'\r' { "Enter" } else { "LF" }, "ordinal": keys}),
                );
                match mode {
                    "confirm" if keys == 1 => surface("CONFIRM1", &message),
                    "retry" if keys <= 3 => surface("RETRY", &message),
                    "queue" if keys <= 2 => surface("QUEUE", &message),
                    _ => {
                        event(
                            &mut log,
                            json!({"kind": "native_accepted", "message": message}),
                        );
                        surface("ACCEPTED", &message);
                    }
                }
            } else if mode == "literal-control" && (0x20..0x7f).contains(&byte) {
                payload.push(byte);
                event(&mut log, json!({"kind": "typed_byte", "byte": byte}));
                if payload == b"/fixture-session" {
                    surface("PASTED", &message);
                }
            } else {
                event(&mut log, json!({"kind": "unexpected_byte", "byte": byte}));
            }
        }
    }
}
