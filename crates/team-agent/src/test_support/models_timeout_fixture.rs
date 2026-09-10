use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const CHILD_FLAG: &str = "--team-agent-models-timeout-child";
const LIST_MODELS_FLAG: &str = "--list-models";
const RECEIPT_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_RECEIPT";
const MODE_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_MODE";
const READY_SOCKET_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_READY_SOCKET";
const STDOUT_MARKER: &[u8] = b"__team_agent_models_timeout_descendant_stdout_v1__\n";

fn receipt_path() -> PathBuf {
    std::env::var_os(RECEIPT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::process::exit(4))
}

fn append_receipt(path: &Path, key: &str, value: impl std::fmt::Display) -> Result<(), ()> {
    let mut receipt = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|_| ())?;
    writeln!(receipt, "{key}={value}").map_err(|_| ())
}

fn child_main(args: &[String]) {
    if args.len() != 1 || args[0] != CHILD_FLAG {
        std::process::exit(5);
    }
    let receipt = receipt_path();
    let socket = std::env::var_os(READY_SOCKET_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::process::exit(5));

    let mut stdout = std::io::stdout();
    if stdout.write_all(STDOUT_MARKER).is_err() || stdout.flush().is_err() {
        std::process::exit(5);
    }
    if append_receipt(&receipt, "child_argv_exact", 1).is_err()
        || append_receipt(&receipt, "descendant_pid", std::process::id()).is_err()
        || append_receipt(&receipt, "descendant_started", 1).is_err()
        || append_receipt(&receipt, "descendant_stdout_inherited", 1).is_err()
    {
        std::process::exit(5);
    }

    let mut ready = UnixStream::connect(socket).unwrap_or_else(|_| std::process::exit(5));
    if ready.write_all(b"1").is_err() {
        std::process::exit(5);
    }
    std::thread::sleep(Duration::from_secs(2));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == CHILD_FLAG) {
        child_main(&args);
        return;
    }
    if args.len() != 1 || args[0] != LIST_MODELS_FLAG {
        std::process::exit(2);
    }

    match std::env::var(MODE_ENV) {
        Ok(mode) if mode == "exit7" => {
            eprintln!("sensitive-token");
            std::process::exit(7);
        }
        Ok(mode) if mode == "oversize" => {
            let mut stdout = std::io::stdout();
            let bytes = [b'x'; 64];
            let _ = stdout.write_all(&bytes);
            let _ = stdout.flush();
            return;
        }
        Ok(mode) if mode == "sleep" => {
            std::thread::sleep(Duration::from_secs(2));
            return;
        }
        Ok(mode) if mode == "descendant" => {}
        Err(std::env::VarError::NotPresent) => {}
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => std::process::exit(2),
    }

    let receipt = receipt_path();
    let socket = PathBuf::from(format!("{}.sock", receipt.display()));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap_or_else(|_| std::process::exit(4));
    if append_receipt(&receipt, "parent_started_once", 1).is_err()
        || append_receipt(&receipt, "parent_argv_exact", 1).is_err()
        || append_receipt(&receipt, "parent_pid", std::process::id()).is_err()
        || append_receipt(&receipt, "child_spawn_attempts", 1).is_err()
    {
        std::process::exit(4);
    }

    let executable = std::env::current_exe().unwrap_or_else(|_| std::process::exit(4));
    let child = Command::new(executable)
        .arg(CHILD_FLAG)
        .env(RECEIPT_ENV, &receipt)
        .env(READY_SOCKET_ENV, &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|_| std::process::exit(4));
    let (mut ready, _) = listener.accept().unwrap_or_else(|_| std::process::exit(4));
    let mut byte = [0_u8; 1];
    if ready.read_exact(&mut byte).is_err() || byte != [b'1'] {
        std::process::exit(4);
    }
    if append_receipt(&receipt, "child_pid", child.id()).is_err() {
        std::process::exit(4);
    }
    let _ = std::fs::remove_file(&socket);
    // The parent exits successfully without waiting; the child retains the
    // inherited stdout pipe and self-exits after the real reader deadline.
}
