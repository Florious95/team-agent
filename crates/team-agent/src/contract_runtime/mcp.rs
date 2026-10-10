//! Owned native stdio entry. Invocation arguments identify an expected record;
//! only actual executable/birth/parent and current instance checks authorize it.
use std::fmt::Display;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use team_agent_contract::contract::types::*;
use team_agent_contract::host::process::{capture_process, sample_process, ProcessState};
use team_agent_contract::orchestration::{
    mcp::*,
    protocol::ConnectionRecord,
    store::{ContractStore, SeatRecord, SeatStatus},
    Error,
};

use super::backend::{Backend, BackendError};

// Diagnostics are not protocol facts or admission. Never write them to stdout,
// include request/argv/env bytes, or let a failed log write change the result.
struct Diagnostics {
    log: Option<crate::event_log::EventLog>,
    fields: Value,
}
impl Diagnostics {
    fn new() -> Self {
        Self {
            log: None,
            fields: json!({
                "pid": std::process::id(),
                "entry_ppid": crate::platform::process::current_parent_pid(),
            }),
        }
    }

    fn bind_log(&mut self, workspace: &Path) {
        let directory = crate::model::paths::logs_dir(workspace);
        let path = directory.join("events.jsonl");
        // Entry diagnostics must not create an unverified workspace, follow a
        // redirected log path, or depend on successfully opening contract.db.
        let regular_or_absent = std::fs::symlink_metadata(&path).map_or_else(
            |error| error.kind() == std::io::ErrorKind::NotFound,
            |metadata| metadata.is_file() && !metadata.file_type().is_symlink(),
        );
        if workspace.is_absolute()
            && workspace.canonicalize().is_ok_and(|path| path == workspace)
            && directory.canonicalize().is_ok_and(|path| path == directory)
            && regular_or_absent
        {
            self.log = Some(crate::event_log::EventLog::new(workspace));
        }
    }

    fn record(&self, stage: &str, outcome: &str, error: Option<&dyn Display>) -> Value {
        let mut value = self.fields.clone();
        value["event"] = json!("contract.mcp");
        value["ts"] = json!(chrono::Utc::now().to_rfc3339());
        value["stage"] = json!(stage);
        value["outcome"] = json!(outcome);
        if let Some(error) = error {
            let text = crate::redaction::redact_external_text(&error.to_string());
            value["error"] = json!(text.chars().take(256).collect::<String>());
            value["error_truncated"] = json!(text.chars().count() > 256);
        }
        crate::redaction::redact_external_value(&value)
    }

    fn emit(&self, stage: &str, outcome: &str, error: Option<&dyn Display>) {
        let value = self.record(stage, outcome, error);
        write_diagnostic(self.log.as_ref(), &value, &mut std::io::stderr().lock());
    }

    fn step<T, E: Display>(
        &self,
        stage: &str,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        self.emit(stage, "started", None);
        let result = action();
        self.emit(
            stage,
            if result.is_ok() { "ok" } else { "error" },
            result.as_ref().err().map(|error| error as &dyn Display),
        );
        result
    }
}

fn write_diagnostic(
    log: Option<&crate::event_log::EventLog>,
    value: &Value,
    stderr: &mut impl Write,
) {
    if log.is_some_and(|log| log.write("contract.mcp", value.clone()).is_ok()) {
        return;
    }
    if let Ok(mut bytes) = serde_json::to_vec(value) {
        // Valid bounded JSON even if future diagnostic fields grow unexpectedly.
        if bytes.len() > 8191 {
            bytes = br#"{"event":"contract.mcp","stage":"diagnostic_size_limit"}"#.to_vec();
        }
        bytes.push(b'\n');
        let _ = stderr.write_all(&bytes);
    }
}

fn arguments(
    args: &[String],
) -> Result<(std::path::PathBuf, String, SeatId, InstanceId, Generation), BackendError> {
    let mut values = std::collections::BTreeMap::new();
    if !args.len().is_multiple_of(2) {
        return Err(BackendError::Metadata);
    }
    for pair in args.chunks_exact(2) {
        if !matches!(
            pair[0].as_str(),
            "--workspace" | "--team" | "--seat" | "--instance" | "--generation"
        ) || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(BackendError::Metadata);
        }
    }
    if values.len() != 5 {
        return Err(BackendError::Metadata);
    }
    let get = |key| values.get(key).copied().ok_or(BackendError::Metadata);
    Ok((
        get("--workspace")?.into(),
        get("--team")?.into(),
        SeatId::new(get("--seat")?)?,
        InstanceId::new(get("--instance")?)?,
        Generation(
            get("--generation")?
                .parse()
                .map_err(|_| BackendError::Metadata)?,
        ),
    ))
}
fn wait_target(store: &ContractStore, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let seat = store.assert_current(identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        if seat.physical.is_some() {
            return Ok(seat);
        }
        if Instant::now() >= deadline {
            return Err(Error::Host("native target publication timed out"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn run(args: &[String]) -> Result<(), BackendError> {
    let mut diagnostics = Diagnostics::new();
    // This is the first action in the main.rs contract-mcp dispatch target.
    // Arguments are not trusted as a log destination yet: stderr only.
    diagnostics.emit("entry", "entered", None);
    let result = run_observed(args, &mut diagnostics);
    diagnostics.emit(
        "exit",
        if result.is_ok() { "ok" } else { "error" },
        result.as_ref().err().map(|error| error as &dyn Display),
    );
    result
}

fn run_observed(args: &[String], diagnostics: &mut Diagnostics) -> Result<(), BackendError> {
    let (workspace, team, seat, instance, generation) =
        diagnostics.step("arguments", || arguments(args))?;
    diagnostics.bind_log(&workspace);
    diagnostics.fields["seat"] = json!(seat.as_str().chars().take(96).collect::<String>());
    diagnostics.fields["instance"] = json!(instance.as_str().chars().take(96).collect::<String>());
    diagnostics.fields["generation"] = json!(generation.0);
    diagnostics.emit("entry", "arguments_validated", None);
    let mut backend = diagnostics.step("backend_open", || Backend::open(&workspace, &team))?;
    let identity = InstanceIdentity {
        scope: backend.binding.scope.clone(),
        seat,
        instance,
        generation,
    };
    diagnostics.fields["scope"] = json!(identity.scope.as_str());
    let seat = diagnostics.step("target_wait", || wait_target(&backend.store, &identity))?;
    let target = seat
        .physical
        .as_ref()
        .ok_or(Error::Invalid("native target missing"))?;
    diagnostics.fields["expected_native_pid"] = json!(target.process.identity.pid);
    diagnostics.emit("target_published", "observed", None);
    let process = diagnostics.step("process_capture", || {
        capture_process(
            std::process::id(),
            &backend.binding.candidate,
            backend.binding.candidate_sha256,
            Duration::from_secs(10),
        )
    })?;
    diagnostics.fields["captured_ppid"] = json!(process.parent);
    // Preserve the existing short-circuit and direct-parent fence. Diagnostic
    // PPID fields do not confer authority or trigger an ancestor scan.
    let parent_matches = process.parent == target.process.identity.pid;
    diagnostics.fields["parent_matches"] = json!(parent_matches);
    let native = parent_matches.then(|| sample_process(&target.process));
    diagnostics.fields["native_process_state"] = json!(match native.as_ref() {
        None => "not_sampled_parent_mismatch",
        Some(ProcessState::Alive) => "alive",
        Some(ProcessState::Exited) => "exited",
        Some(ProcessState::Replaced) => "replaced",
        Some(ProcessState::Unknown(_)) => "unknown",
    });
    if let Some(ProcessState::Unknown(error)) = &native {
        diagnostics.emit("native_process_sample", "error", Some(error));
    }
    diagnostics.step("parent_fence", || {
        if !parent_matches || native != Some(ProcessState::Alive) {
            Err(Error::Fence)
        } else {
            Ok(())
        }
    })?;
    let connection = diagnostics
        .step("connection_identity", || {
            backend.store.propose_identity(&identity.seat)
        })?
        .instance;
    let record = ConnectionRecord {
        identity: identity.clone(),
        connection: connection.clone(),
        server_key: seat.server_key.clone(),
        binding_key: seat.binding_key.clone(),
        process,
        native_process: target.process.clone(),
        closed: false,
    };
    diagnostics.step("register_connection", || {
        backend.store.register_connection(&record)
    })?;
    // Registration is not an initialize/tools-list/client-consumption fact.
    // Those continue to be recorded only at the existing protocol boundaries.
    let result = diagnostics.step("serve", || {
        serve_with_context(
            &mut backend.store,
            |store, request| {
                if sample_process(&record.process) != ProcessState::Alive
                    || sample_process(&record.native_process) != ProcessState::Alive
                {
                    return Err(Error::Fence);
                }
                let current = store.assert_current(&identity)?;
                if current.binding_key != record.binding_key
                    || matches!(current.status, SeatStatus::Stopped | SeatStatus::Unknown)
                    || current
                        .physical
                        .as_ref()
                        .is_none_or(|target| target.process != record.native_process)
                {
                    return Err(Error::Fence);
                }
                let business =
                    request.get("method").and_then(serde_json::Value::as_str) == Some("tools/call");
                let mut task = String::new();
                if business {
                    // A native callback can race the physical receipt's DB commit. Wait
                    // for that exact generation's submitted turn, never a queued row.
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        match store.submitted_task(&identity) {
                            Ok(Some(bound)) => {
                                task = bound;
                                break;
                            }
                            Ok(None) | Err(Error::Conflict) if Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(20))
                            }
                            Ok(None) => return Err(Error::Invalid("no submitted native turn")),
                            Err(error) => return Err(error),
                        }
                    }
                }
                if sample_process(&record.process) != ProcessState::Alive
                    || sample_process(&record.native_process) != ProcessState::Alive
                {
                    return Err(Error::Fence);
                }
                Ok(CallContext {
                    identity: identity.clone(),
                    binding_key: record.binding_key.clone(),
                    connection_id: connection.clone(),
                    task_id: task,
                })
            },
            std::io::stdin().lock(),
            std::io::stdout().lock(),
        )
    });
    let closed = diagnostics.step("close_connection", || {
        backend.store.close_connection(&identity, &connection)
    });
    result?;
    closed?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_preserve_errors_and_distinguish_captured_and_expected_parent() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.fields["captured_ppid"] = json!(202);
        diagnostics.fields["expected_native_pid"] = json!(303);
        diagnostics.fields["parent_matches"] = json!(false);
        let result = diagnostics.step("parent_fence", || Err::<(), _>(Error::Fence));
        assert_eq!(result, Err(Error::Fence));
        let event = diagnostics.record("parent_fence", "error", Some(&Error::Fence));
        assert_eq!(event["captured_ppid"], 202);
        assert_eq!(event["expected_native_pid"], 303);
        assert_eq!(event["parent_matches"], false);
        assert_eq!(event["error"], Error::Fence.to_string());
        assert!(event["entry_ppid"].is_number());
    }

    #[test]
    fn diagnostics_redact_and_bound_error_without_argument_or_environment_fields() {
        let diagnostics = Diagnostics::new();
        let error = format!("PASSWORD=do-not-log {}", "x".repeat(20_000));
        let event = diagnostics.record("arguments", "error", Some(&error));
        assert_eq!(event["error_truncated"], true);
        assert!(!event.to_string().contains("do-not-log"));
        for field in ["argv", "args", "env", "prompt", "request", "content"] {
            assert!(event.get(field).is_none());
        }
        let mut stderr = Vec::new();
        write_diagnostic(None, &event, &mut stderr);
        assert!(stderr.len() <= 8192);
        assert_eq!(serde_json::from_slice::<Value>(&stderr).unwrap(), event);
    }

    #[test]
    fn diagnostic_sink_failures_do_not_change_execution_results() {
        let log = crate::event_log::EventLog::at("/dev/null/contract-mcp-events".into());
        let diagnostics = Diagnostics::new();
        let event = diagnostics.record("backend_open", "error", Some(&Error::Fence));
        let mut stderr = Vec::new();
        write_diagnostic(Some(&log), &event, &mut stderr);
        assert_eq!(serde_json::from_slice::<Value>(&stderr).unwrap(), event);
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        write_diagnostic(Some(&log), &event, &mut Broken);
        assert_eq!(diagnostics.step("fixture", || Ok::<_, Error>(17)), Ok(17));
        stderr.clear();
        write_diagnostic(
            None,
            &json!({"unexpected": "x".repeat(20_000)}),
            &mut stderr,
        );
        let bounded: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(bounded["stage"], "diagnostic_size_limit");
        assert!(stderr.len() <= 8192);
    }
}
