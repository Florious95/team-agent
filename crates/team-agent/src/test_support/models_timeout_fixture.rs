use std::process::{Command, Stdio};
use std::time::Duration;

const CHILD_FLAG: &str = "--team-agent-models-timeout-child";
const LIST_MODELS_FLAG: &str = "--list-models";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == CHILD_FLAG) {
        std::thread::sleep(Duration::from_secs(2));
        return;
    }
    if !args.iter().any(|arg| arg == LIST_MODELS_FLAG) {
        std::process::exit(2);
    }

    let executable = std::env::current_exe().expect("timeout helper current_exe");
    let spawned = Command::new(executable)
        .arg(CHILD_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn();
    if spawned.is_err() {
        std::process::exit(3);
    }
    // Do not wait: the parent exits successfully while its child retains the
    // inherited stdout pipe, exercising the real runner's EOF deadline.
}
