use std::collections::BTreeSet;
use std::ffi::OsStr;

use super::{HostError, HostErrorKind};
use crate::contract::plan::LaunchPlan;

/// Byte-preserving POSIX single quoting. This is not JSON/YAML or a provider argv builder.
/// The legacy helper is private/provider-coupled; no old module is imported or changed.
pub fn quote(value: &OsStr) -> Result<Vec<u8>, HostError> {
    let bytes = value.as_encoded_bytes();
    if bytes.contains(&0) {
        return Err(HostError::new("shell NUL", HostErrorKind::Invalid));
    }
    let mut output = Vec::with_capacity(bytes.len() + 2);
    output.push(b'\'');
    for byte in bytes {
        if *byte == b'\'' {
            output.extend_from_slice(b"'\\''");
        } else {
            output.push(*byte);
        }
    }
    output.push(b'\'');
    Ok(output)
}

/// The owned tmux pane execs the native program: pane_pid becomes the real native PID.
/// remain-on-exit is set in the private server's config before spawn; the tmux server,
/// not screen text or a second shell, owns wait/exit facts. Dead panes never take input.
pub fn launch_script(
    plan: &LaunchPlan,
    inherited_identity_keys: &BTreeSet<String>,
) -> Result<Vec<u8>, HostError> {
    launch_script_with_worker(plan, inherited_identity_keys, None)
}

pub(crate) struct WorkerContext<'a> {
    pub workspace: &'a std::path::Path,
    pub team: &'a str,
    pub seat: &'a str,
    pub provider: &'a str,
    pub endpoint: &'a std::path::Path,
}

pub(crate) fn launch_script_with_worker(
    plan: &LaunchPlan,
    inherited_identity_keys: &BTreeSet<String>,
    worker: Option<&WorkerContext<'_>>,
) -> Result<Vec<u8>, HostError> {
    plan.environment
        .validate()
        .map_err(|_| HostError::new("launch environment", HostErrorKind::Invalid))?;
    let mut script = b"#!/bin/sh\numask 077\ncd ".to_vec();
    script.extend(quote(plan.cwd.as_os_str())?);
    script.extend_from_slice(b" || exit 126\n");
    if inherited_identity_keys
        .iter()
        .any(|key| !crate::contract::types::valid_env_key(key))
    {
        return Err(HostError::new(
            "inherited identity key",
            HostErrorKind::Invalid,
        ));
    }
    // A borrowed tmux server can contain identity keys absent from the caller.
    // Enumerate only TEAM_AGENT_ names inside the child; opaque auth/proxy values
    // never leave this pipeline or enter a host receipt. No eval or value export.
    script.extend_from_slice(br#"[ -x /usr/bin/env ] && [ -x /usr/bin/sed ] || exit 126
TEAM_AGENT_HOST_NAMES=$(/usr/bin/env | /usr/bin/sed -n 's/^\(TEAM_AGENT_[A-Za-z0-9_]*\)=.*/\1/p') || exit 126
for TEAM_AGENT_HOST_KEY in $TEAM_AGENT_HOST_NAMES; do unset "$TEAM_AGENT_HOST_KEY" || exit 126; done
unset TEAM_AGENT_HOST_NAMES TEAM_AGENT_HOST_KEY
"#);
    // Clear inherited framework identities first; only the explicit captured overlay wins.
    for key in plan.environment.remove.union(inherited_identity_keys) {
        script.extend_from_slice(b"unset ");
        script.extend_from_slice(key.as_bytes());
        script.push(b'\n');
    }
    for (key, value) in &plan.environment.set {
        script.extend_from_slice(b"export ");
        script.extend_from_slice(key.as_bytes());
        script.push(b'=');
        script.extend(quote(value)?);
        script.push(b'\n');
    }
    if let Some(worker) = worker {
        // Only a host-verified workspace binding may mint a new caller tuple.
        // TMUX_PANE is supplied by the newly created pane, never the launcher.
        script.extend_from_slice(b"test -n \"${TMUX_PANE:-}\" || exit 126\n");
        for (key, value) in [
            ("TEAM_AGENT_WORKSPACE", worker.workspace.as_os_str()),
            ("TEAM_AGENT_TEAM_ID", OsStr::new(worker.team)),
            ("TEAM_AGENT_OWNER_TEAM_ID", OsStr::new(worker.team)),
            ("TEAM_AGENT_ID", OsStr::new(worker.seat)),
            ("TEAM_AGENT_CALLER_PROVIDER", OsStr::new(worker.provider)),
            ("TEAM_AGENT_CALLER_TMUX_ENDPOINT", worker.endpoint.as_os_str()),
        ] {
            script.extend_from_slice(format!("export {key}=").as_bytes());
            script.extend(quote(value)?);
            script.push(b'\n');
        }
        script.extend_from_slice(b"export TEAM_AGENT_CALLER_PANE_ID=\"$TMUX_PANE\"\n");
    }
    // Exec the public command by bare name, exactly as the user's shell would:
    // `sh` resolves it through PATH, so wrappers on PATH run and keep their name.
    plan.launcher
        .validate()
        .map_err(|_| HostError::new("public launch command", HostErrorKind::Invalid))?;
    script.extend_from_slice(b"export PATH=");
    script.extend(quote(&plan.launcher.search_path)?);
    script.extend_from_slice(b"\nexec ");
    script.extend(quote(std::ffi::OsStr::new(&plan.launcher.command))?);
    for argument in &plan.arguments {
        script.push(b' ');
        script.extend(quote(argument)?);
    }
    script.push(b'\n');
    Ok(script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::plan::{CarrierReport, CarrierUse, EnvironmentDelta, ExpectedSession, LaunchEntrypoint};
    use crate::contract::types::Digest;

    #[test]
    fn workspace_worker_gets_fresh_caller_identity_not_inherited_identity() {
        let plan = LaunchPlan {
            executable: "/usr/bin/env".into(),
            launcher: LaunchEntrypoint {
                command: "env".into(), search_path: "/usr/bin:/bin".into(),
                executable: "/usr/bin/env".into(), sha256: Digest([0; 32]),
            },
            arguments: vec![], executable_inputs: vec![], cwd: "/".into(),
            environment: EnvironmentDelta { remove: Default::default(), set: Default::default() },
            expected_session: ExpectedSession::CaptureAfterLaunch, materialization: vec![],
            carriers: CarrierReport { model: CarrierUse::NotRequested, prompt: CarrierUse::NotRequested,
                mcp: CarrierUse::NotRequested, bypass: CarrierUse::NotRequested, effort: CarrierUse::NotRequested },
        };
        let worker = WorkerContext {
            workspace: std::path::Path::new("/owned workspace"), team: "parent", seat: "manager",
            provider: "codex", endpoint: std::path::Path::new("/tmp/owned.sock"),
        };
        for context in [None, Some(&worker)] {
            let script = launch_script_with_worker(&plan, &Default::default(), context).unwrap();
            let result = std::process::Command::new("/bin/sh")
                .arg("-c").arg(String::from_utf8(script).unwrap()).env_clear()
                .env("TMUX_PANE", "%7").env("TMUX", "/tmp/owned.sock,1,0")
                .env("TEAM_AGENT_OWNER_TEAM_ID", "foreign")
                .env("TEAM_AGENT_CALLER_PANE_ID", "%99")
                .env("TEAM_AGENT_UNRELATED", "stale").output().unwrap();
            assert!(result.status.success());
            let output = String::from_utf8(result.stdout).unwrap();
            assert!(!output.contains("foreign") && !output.contains("%99") && !output.contains("TEAM_AGENT_UNRELATED"));
            if context.is_some() {
                for expected in ["TEAM_AGENT_WORKSPACE=/owned workspace", "TEAM_AGENT_TEAM_ID=parent",
                    "TEAM_AGENT_OWNER_TEAM_ID=parent", "TEAM_AGENT_ID=manager", "TEAM_AGENT_CALLER_PROVIDER=codex",
                    "TEAM_AGENT_CALLER_PANE_ID=%7", "TEAM_AGENT_CALLER_TMUX_ENDPOINT=/tmp/owned.sock"] {
                    assert!(output.lines().any(|line| line == expected), "{expected}: {output}");
                }
            } else {
                assert!(!output.contains("TEAM_AGENT_"));
            }
        }
    }
}
