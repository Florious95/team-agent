//! Shared CLI/MCP send semantics. Durable storage precedes optional physical delivery.
use crate::messaging::presentation::{PresentationRequest, PresentationSink};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone, Copy)]
pub enum Authority<'a> {
    Operator { sender: &'a str },
    Seat { sender: &'a str, team: &'a str },
}
impl<'a> Authority<'a> {
    fn sender(self) -> &'a str {
        match self {
            Self::Operator { sender } | Self::Seat { sender, .. } => sender,
        }
    }
}

pub struct Request<'a> {
    pub workspace: &'a Path,
    pub team: Option<&'a str>,
    /// One recipient, a comma-separated fanout, `*`, or an operator team/seat address.
    pub to: &'a str,
    pub content: &'a str,
    pub task_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub presentation: &'a PresentationRequest,
    pub mailbox: bool,
    pub envelope: Option<&'a Value>,
}

/// Escape terminal control bytes only in the live presentation. The inbox retains
/// the original string and envelope, including every newline and unknown field.
pub fn render(sender: &str, content: &str, message_id: &str, result: bool) -> String {
    const MAX: usize = 65_536;
    let kind = if result { "result" } else { "message" };
    let mut text =
        format!("Team Agent {kind} from {sender}:\n\n{content}").replace('\u{001b}', "␛");
    if text.len() > MAX {
        let notice = format!("\n[truncated; full message {message_id} is stored in inbox]\n");
        let mut end = MAX - notice.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&notice);
    }
    text
}

pub fn execute(req: Request<'_>, authority: Authority<'_>) -> Result<Value, String> {
    let presentation = serde_json::to_value(req.presentation).map_err(|e| e.to_string())?;
    crate::messaging::presentation::normalize_send_presentation(None, Some(&presentation))?;
    let bound_team = match authority {
        Authority::Seat { team, .. } => Some(team),
        _ => None,
    };
    let selected = req.team.or(bound_team);
    let authorized = bound_team
        .map(|team| crate::direct_delivery::directory(req.workspace, Some(team)))
        .transpose()?;
    if let Some(bound) = &authorized {
        if crate::direct_delivery::directory(req.workspace, selected)?.team != bound.team {
            return Err("seat cannot send across team scope".into());
        }
    }
    let addresses: Vec<_> = req.to.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if addresses.is_empty() {
        return Err("send requires at least one recipient".into());
    }
    let mut batch = addresses.len() > 1;
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for address in addresses {
        let outcome = (|| -> Result<Vec<Value>, String> {
            let parsed = crate::messaging::parse_logical_address(address)
                .map_err(|reason| format!("invalid recipient name: {address}: {reason}"))?;
            let team = if parsed.workspace.is_some() { None } else { selected };
            let workspace = parsed.workspace.as_deref().unwrap_or(req.workspace);
            let (directory, recipient) = match parsed.target {
                crate::messaging::LogicalAddressTarget::Worker(agent) =>
                    (crate::direct_delivery::directory(workspace, team)?, agent),
                crate::messaging::LogicalAddressTarget::TeamEntity { team, entity } =>
                    (crate::direct_delivery::directory(workspace, Some(&team))?, entity),
                crate::messaging::LogicalAddressTarget::SessionWindow { .. } =>
                    return Err("Terminal-window addressing is no longer supported; use an agent name or leader, with --workspace/--team to select scope".into()),
            };
            if let Some(bound) = &authorized {
                if directory.workspace != bound.workspace || directory.team != bound.team {
                    return Err("seat cannot send across workspace or team scope".into());
                }
            }
            let broadcast = recipient == "*";
            let recipients = if broadcast {
                // Attribution is qualified across workspace boundaries; self-exclusion
                // compares the originating seat only inside its actual directory.
                let sender = match crate::messaging::parse_logical_address(authority.sender()) {
                    Ok(crate::messaging::LogicalAddress {
                        workspace: Some(workspace),
                        target: crate::messaging::LogicalAddressTarget::TeamEntity { team, entity },
                        ..
                    }) if directory.team.as_deref() == Some(team.as_str())
                        && crate::model::paths::canonical_run_workspace(&workspace)
                            .is_ok_and(|workspace| workspace == directory.workspace) => entity,
                    _ => authority.sender().to_string(),
                };
                directory.agents().into_iter().filter(|id| id != &sender).collect()
            } else { vec![recipient] };
            if recipients.is_empty() {
                return Err("send requires at least one recipient".into());
            }
            batch |= broadcast;
            let mut values = Vec::new();
            for recipient in recipients {
                if !seen.insert((directory.workspace.clone(), directory.team.clone(), recipient.clone())) {
                    continue;
                }
                match send_one(&req, authority, &directory, &recipient) {
                    Ok(value) => values.push(value),
                    Err(error) if batch => values.push(json!({"recipient":recipient,"ok":false,"error":error})),
                    Err(error) => return Err(error),
                }
            }
            Ok(values)
        })();
        match outcome {
            Ok(values) => results.extend(values),
            Err(error) if batch => results.push(json!({"recipient":address,"ok":false,"error":error})),
            Err(error) => return Err(error),
        }
    }
    if !batch {
        return Ok(results.remove(0));
    }
    let sent = results
        .iter()
        .filter(|r| r["delivery"] == "physical")
        .count();
    let stored = results.iter().filter(|r| r["stored"] == true).count();
    let failed = results
        .iter()
        .any(|r| r["ok"] == false || r.get("delivery_error").is_some());
    Ok(
        json!({"ok":stored > 0,"status":if failed {"partial"} else if sent > 0 {"sent"} else {"stored"},
        "sent":sent,"stored":stored,"messages":results}),
    )
}

fn send_one(
    req: &Request<'_>,
    authority: Authority<'_>,
    directory: &crate::direct_delivery::Directory,
    recipient: &str,
) -> Result<Value, String> {
    if recipient != "leader" && !directory.agents().iter().any(|id| id == recipient) {
        return Err(format!("unknown recipient: {recipient}"));
    }
    let sender = authority.sender();
    let kind = if req.envelope.is_some() {
        "result"
    } else {
        "message"
    };
    let mut record = crate::inbox_store::record(
        sender,
        recipient,
        req.content,
        directory.team.as_deref(),
        kind,
    );
    record["presentation"] = serde_json::to_value(req.presentation).map_err(|e| e.to_string())?;
    record["provenance"] = json!({"authority":match authority { Authority::Operator {..} => "operator", Authority::Seat {..} => "seat" },"sender":sender,"team":directory.team});
    if let Some(task) = req.task_id {
        record["task_id"] = json!(task);
    }
    if let Some(id) = req.message_id {
        if id.starts_with("ss1:") {
            record["dedupe_key"] = json!(id);
        } else {
            record["message_id"] = json!(id);
            record["request_id"] = json!(id);
        }
    }
    if let Some(envelope) = req.envelope {
        record["envelope"] = envelope.clone();
        // Scope, caller and normalized business payload define a report retry. No
        // generated timestamp or process id can turn a retry into another report.
        let mut payload = envelope.clone();
        if let Some(fields) = payload.as_object_mut() {
            if let Some(token) = fields.remove("dedupe_key") {
                if !token.is_string() {
                    return Err("invalid_dedupe_key".into());
                }
                record["dedupe_key"] = token;
            }
            fields.remove("record_ref");
        }
        let mut identity = json!([
            directory.workspace,
            directory.team,
            sender,
            recipient,
            payload
        ]);
        identity.sort_all_objects();
        record["request_id"] = json!(format!(
            "result-{:x}",
            Sha256::digest(serde_json::to_vec(&identity).map_err(|e| e.to_string())?)
        ));
    }
    let stored = crate::inbox_store::store(&directory.workspace, recipient, &record)?;
    let id = stored.record["message_id"]
        .as_str()
        .ok_or("stored record has no message id")?;
    let mut result = json!({"ok":true,"status":"stored","stored":true,"recipient":recipient,
        "message_id":id,"accepted":null,"duplicate":stored.duplicate,"live":false,
        "record_ref":stored.reference,"dedupe_key":stored.reference.token(),"anonymous_dedupe_window":crate::inbox_store::DEDUPE_WINDOW});
    if let Some(envelope) = req.envelope {
        result["task_id"] = envelope["task_id"].clone();
        result["agent_id"] = envelope["agent_id"].clone();
        result["presentation"] = record["presentation"].clone();
    }
    if stored.duplicate || req.mailbox || req.presentation.sink != PresentationSink::Leader {
        return Ok(result);
    }
    let text = render(sender, req.content, id, req.envelope.is_some());
    match directory.send_with_receipt(recipient, &text, || {
        if req.envelope.is_none() {
            if let Some(task) = req.task_id {
                remember_task(directory, recipient, task, id)?;
            }
        }
        Ok(())
    }) {
        Ok(delivery) => {
            result["status"] = json!(if req.envelope.is_some() {
                "reported"
            } else {
                "sent"
            });
            result["live"] = json!(true);
            result["delivery"] = json!("physical");
            for key in ["pane_id", "dialect"] {
                if let Some(value) = delivery.get(key) {
                    result[key] = value.clone();
                }
            }
        }
        Err(error) => {
            result["submission"] = json!(if error.starts_with("uncertain:") {
                "uncertain"
            } else {
                "not_attempted"
            });
            result["delivery_error"] = json!(error);
        }
    }
    Ok(result)
}

fn remember_task(
    directory: &crate::direct_delivery::Directory,
    recipient: &str,
    task: &str,
    message_id: &str,
) -> Result<(), String> {
    use std::io::Write;
    let path =
        crate::inbox_store::task_path(&directory.workspace, recipient, directory.team.as_deref())?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let value = json!({"v":1,"task_id":task,"message_id":message_id,
        "endpoint":directory.endpoint(recipient)?,
        "pid":directory.state.pointer(&format!("/agents/{recipient}/pid")).unwrap_or(&Value::Null)});
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().unwrap())
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// An advisory, already-submitted task may label a result. Absence is never a
/// readiness gate, and no coordinator/SQLite query is required to report work.
pub fn submitted_task(workspace: &Path, team: &str, agent: &str) -> Option<String> {
    let directory = crate::direct_delivery::directory(workspace, Some(team)).ok()?;
    if let Ok(path) =
        crate::inbox_store::task_path(&directory.workspace, agent, directory.team.as_deref())
    {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                let endpoint = directory
                    .endpoint(agent)
                    .ok()
                    .and_then(|e| serde_json::to_value(e).ok());
                if value["v"] == 1
                    && endpoint.as_ref() == value.get("endpoint")
                    && value.get("pid")
                        == directory
                            .state
                            .pointer(&format!("/agents/{agent}/pid"))
                            .or(Some(&Value::Null))
                {
                    if let Some(task) = value["task_id"].as_str().filter(|s| !s.is_empty()) {
                        return Some(task.to_string());
                    }
                }
            }
        }
    }
    let seat = directory.state.get("agents")?.get(agent)?;
    let value = seat.get("last_submitted_task")?;
    value
        .as_str()
        .or_else(|| value.get("task_id").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
