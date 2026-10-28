use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_FLAG: &str = "--team-agent-models-timeout-child";
const LIST_MODELS_FLAG: &str = "--list-models";
const RECEIPT_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_RECEIPT";
const STAGE_RECEIPT_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_STAGE_RECEIPT";
const MODE_ENV: &str = "TEAM_AGENT_MODELS_TIMEOUT_MODE";
const STDOUT_MARKER: &[u8] = b"__team_agent_models_timeout_descendant_stdout_v1__\n";
const PARENT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(1);

// Stage values are UNIX epoch milliseconds so the test process and helper use
// one directly comparable clock. Stage writes are best-effort diagnostics and
// never change the fixture's production-facing exit semantics.
fn append_stage(key: &str) {
    let Some(path) = std::env::var_os(STAGE_RECEIPT_ENV) else {
        return;
    };
    let Some(milliseconds) = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
    else {
        return;
    };
    let _ = append_receipt(Path::new(&path), key, milliseconds);
}

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

fn wait_for_parent_handoff(receipt: &Path) {
    let expected = format!("child_pid={}", std::process::id());
    let deadline = Instant::now() + PARENT_HANDOFF_TIMEOUT;
    loop {
        if std::fs::read_to_string(receipt)
            .ok()
            .is_some_and(|text| text.lines().any(|line| line == expected))
        {
            return;
        }
        if Instant::now() >= deadline {
            std::process::exit(5);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn child_main(args: &[String]) {
    if args.len() != 1 || args[0] != CHILD_FLAG {
        std::process::exit(5);
    }
    let receipt = receipt_path();
    wait_for_parent_handoff(&receipt);

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

    append_stage("parent_entry_ms");
    let receipt = receipt_path();
    if append_receipt(&receipt, "parent_started_once", 1).is_err()
        || append_receipt(&receipt, "parent_argv_exact", 1).is_err()
        || append_receipt(&receipt, "parent_pid", std::process::id()).is_err()
        || append_receipt(&receipt, "child_spawn_attempts", 1).is_err()
    {
        std::process::exit(4);
    }

    let executable = std::env::current_exe().unwrap_or_else(|_| std::process::exit(4));
    append_stage("parent_before_child_spawn_ms");
    let child = Command::new(executable)
        .arg(CHILD_FLAG)
        .env(RECEIPT_ENV, &receipt)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|_| std::process::exit(4));
    append_stage("parent_after_child_spawn_ms");
    if append_receipt(&receipt, "child_pid", child.id()).is_err() {
        std::process::exit(4);
    }
    append_stage("parent_before_exit_ms");
    // The parent exits successfully without waiting; the child retains the
    // inherited stdout pipe and self-exits after the real reader deadline.
}
