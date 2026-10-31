//! Shared CLI/MCP send semantics. Durable storage precedes optional physical delivery.
use crate::direct_delivery::AddressError;
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

fn same_workspace(left: &Path, right: &Path) -> bool {
    left == right || left.canonicalize().ok().zip(right.canonicalize().ok())
        .is_some_and(|(left, right)| left == right)
}

fn sender_in_directory(authority: Authority<'_>, directory: &crate::direct_delivery::Directory) -> String {
    match crate::messaging::parse_logical_address(authority.sender()) {
        Ok(crate::messaging::LogicalAddress {
            workspace: Some(workspace),
            target: crate::messaging::LogicalAddressTarget::TeamEntity { team, entity },
            ..
        }) if directory.team.as_deref() == Some(team.as_str())
            && crate::model::paths::canonical_run_workspace(&workspace)
                .is_ok_and(|workspace| same_workspace(&workspace, &directory.workspace)) => entity,
        _ => authority.sender().to_string(),
    }
}

fn permitted_peer(authority: Authority<'_>, directory: &crate::direct_delivery::Directory, recipient: &str) -> bool {
    if recipient == "leader" { return true; }
    let Some(info) = directory.state.get("agents").and_then(|agents| agents.get(recipient)) else { return false; };
    !matches!(authority, Authority::Seat { .. })
        || !matches!(info["status"].as_str().unwrap_or("").to_ascii_lowercase().as_str(), "dead" | "stopped")
}

pub fn execute(req: Request<'_>, authority: Authority<'_>) -> Result<Value, String> {
    let presentation = serde_json::to_value(req.presentation).map_err(|e| e.to_string())?;
    if req.envelope.is_some() {
        if let Some(error) = crate::messaging::presentation::normalize_report_presentation(Some(&presentation)).1 {
            return Err(error);
        }
    } else {
        crate::messaging::presentation::normalize_send_presentation(None, Some(&presentation))?;
    }
    let bound_team = match authority {
        Authority::Seat { team, .. } => Some(team),
        _ => None,
    };
    let selected = req.team.or(bound_team);
    let authorized = bound_team
        .map(|team| crate::direct_delivery::directory(req.workspace, Some(team)))
        .transpose()?;
    let scope_check = authorized.as_ref().map_or(Ok(()), |bound| {
        crate::direct_delivery::directory_with_reason(req.workspace, selected).and_then(|directory| {
            if directory.team == bound.team { Ok(()) }
            else { Err(AddressError::new("peer_not_in_scope", "seat cannot widen team scope")) }
        })
    });
    let addresses: Vec<_> = req.to.split(',').map(str::trim).collect();
    let mut batch = addresses.len() > 1;
    let mut admitted = Vec::new();
    let mut invalid = Vec::new();
    let mut invalid_reasons = Vec::new();
    let mut invalid_details = Vec::new();
    let mut not_attempted = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Admission is read-only and completes for the entire request before cleanup,
    // inbox append or physical submission. Never resolve an address again later.
    for address in addresses {
        let outcome = (|| -> Result<(crate::direct_delivery::Directory, Vec<String>, bool), AddressError> {
            scope_check.clone()?;
            let parsed = crate::messaging::parse_logical_address(address).map_err(|e| AddressError::new(
                if req.to.trim().is_empty() { "empty_target_list" } else { "name_invalid" }, e))?;
            let team = if parsed.workspace.is_some() { None } else { selected };
            let workspace = parsed.workspace.as_deref().unwrap_or(req.workspace);
            if let Some(bound) = &authorized {
                if !same_workspace(&crate::model::paths::canonical_run_workspace(workspace)
                    .map_err(|e| AddressError::new("internal_runtime_error", e.to_string()))?, &bound.workspace) {
                    return Err(AddressError::new("peer_not_in_scope", "seat cannot widen workspace scope"));
                }
            }
            let (directory, recipient) = match parsed.target {
                crate::messaging::LogicalAddressTarget::Worker(agent) =>
                    (crate::direct_delivery::directory_with_reason(workspace, team)?, agent),
                crate::messaging::LogicalAddressTarget::TeamEntity { team, entity } =>
                    (crate::direct_delivery::directory_with_reason(workspace, Some(&team))?, entity),
                crate::messaging::LogicalAddressTarget::SessionWindow { .. } =>
                    return Err(AddressError::new("name_invalid", "terminal-window addressing is unsupported")),
            };
            if authorized.as_ref().is_some_and(|bound| directory.team != bound.team) {
                return Err(AddressError::new("peer_not_in_scope", "seat cannot widen team scope"));
            }
            let broadcast = recipient == "*";
            let sender = sender_in_directory(authority, &directory);
            let recipients = if broadcast {
                directory.agents().into_iter().filter(|id| id != &sender && permitted_peer(authority, &directory, id)).collect()
            } else { vec![recipient] };
            if recipients.is_empty() { return Err(AddressError::new("empty_target_list", "no eligible recipients")); }
            for recipient in &recipients {
                if recipient != "leader" && directory.state.get("agents").and_then(|agents| agents.get(recipient)).is_none() {
                    use crate::model::name_similarity::{rank, Candidate};
                    let mut error = AddressError::new("name_not_resolvable", format!("recipient {recipient:?} is not resolvable"));
                    let peers: Vec<_> = directory.agents().into_iter()
                        .filter(|id| id != &sender && permitted_peer(authority, &directory, id))
                        .map(|id| Candidate { match_key: id.clone(), stable_key: id.clone(), payload: id }).collect();
                    error.candidates = rank(recipient, &peers).into_iter().map(|id| {
                        let name = directory.team.as_ref().map_or_else(|| id.clone(), |team| format!("{team}/{id}"));
                        let name = if parsed.workspace.is_some() { format!("{}::{name}", directory.workspace.display()) } else { name };
                        json!({"name":name,"team_key":directory.team,"agent_id":id,"advisory":true})
                    }).collect();
                    return Err(error);
                }
                // An unbound operator's default label is not a seat identity.
                // report_result routes implicitly to the leader, rather than
                // accepting an explicit user-selected self target.
                if !broadcast && !(req.envelope.is_some() && recipient == "leader") && recipient == &sender
                    && !matches!(authority, Authority::Operator { sender: "leader" }) {
                    return Err(AddressError::new("self_target", "explicit self target"));
                }
                if !permitted_peer(authority, &directory, recipient) {
                    return Err(AddressError::new("name_not_live", format!("recipient {recipient:?} is stopped or dead")));
                }
            }
            Ok((directory, recipients, broadcast))
        })();
        match outcome {
            Ok((directory, mut recipients, broadcast)) => {
                batch |= broadcast;
                let workspace_key = directory.workspace.canonicalize().unwrap_or_else(|_| directory.workspace.clone());
                recipients.retain(|recipient| seen.insert((workspace_key.clone(), directory.team.clone(), recipient.clone())));
                not_attempted.extend(recipients.iter().map(|recipient| if broadcast { recipient.clone() } else { address.to_string() }));
                admitted.push((directory, recipients));
            }
            Err(mut error) => {
                if let Some(bound) = &authorized {
                    error.candidates.retain(|candidate| candidate["team_key"].as_str() == bound.team.as_deref());
                }
                let available: Vec<_> = error.candidates.iter().filter_map(|candidate| candidate["team_key"].as_str().map(str::to_string)).collect();
                let mut requested_team = None;
                if matches!(error.code, "team_key_not_found" | "name_ambiguous") {
                    if let Ok(parsed) = crate::messaging::parse_logical_address(address) {
                        let entity = match parsed.target {
                            crate::messaging::LogicalAddressTarget::TeamEntity { team, entity } => { requested_team = Some(team); entity },
                            crate::messaging::LogicalAddressTarget::Worker(entity) => { requested_team = selected.map(str::to_string); entity },
                            _ => String::new(),
                        };
                        if error.code == "team_key_not_found" {
                            use crate::model::name_similarity::{rank, Candidate};
                            let teams: Vec<_> = error.candidates.into_iter().map(|value| {
                                let key = value["team_key"].as_str().unwrap_or("").to_string();
                                Candidate { match_key: key.clone(), stable_key: key, payload: value }
                            }).collect();
                            error.candidates = rank(requested_team.as_deref().unwrap_or(""), &teams);
                        }
                        for candidate in &mut error.candidates {
                            let name = format!("{}/{entity}", candidate["team_key"].as_str().unwrap_or(""));
                            candidate["name"] = json!(parsed.workspace.as_ref().map_or_else(|| name.clone(), |workspace| format!("{}::{name}", workspace.display())));
                            candidate["agent_id"] = json!(entity);
                        }
                    }
                }
                if error.code != "name_ambiguous" {
                    if let Some(name) = error.candidates.first().and_then(|candidate| candidate["name"].as_str()) {
                        error.message.push_str(&format!("; did you mean {name:?}?"));
                    }
                }
                invalid.push(address.to_string());
                invalid_reasons.push(error.message.clone());
                invalid_details.push(json!({"target":address,"reason":error.code,"error_code":error.code,
                    "message":error.message,"candidates":error.candidates,"requested_team":requested_team,
                    "available_team_keys":available}));
            }
        }
    }
    if let Some(first) = invalid_details.first() {
        let code = first["reason"].as_str().unwrap();
        let exception: String = code.split('_').map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }).collect();
        let message = &first["message"];
        let hint = format!("No targets were attempted. {} Correct invalid_targets before retrying.", message.as_str().unwrap());
        return Ok(json!({"ok":false,"status":"refused","reason":code,
            "error_code":code,"exc_type":exception,"message":message,"error":message,"requested_name":invalid[0],
            "invalid_targets":invalid,"invalid_reasons":invalid_reasons,"invalid_details":invalid_details,
            "not_attempted":not_attempted,"candidates":first["candidates"],"suggested_name":first["candidates"][0]["name"],
            "requested_team":first["requested_team"],"available_team_keys":first["available_team_keys"],
            "stored":false,"accepted":null,"hint":hint,"action":hint}));
    }
    // Execution faults belong to their recipient. Already-admitted siblings run
    // independently, with the existing durability and native submission receipts.
    let mut results = Vec::new();
    for (directory, recipients) in admitted {
        for recipient in recipients {
            match send_one(&req, authority, &directory, &recipient) {
                Ok(value) => results.push(value),
                Err(error) if batch => results.push(json!({"recipient":recipient,"ok":false,"error":error})),
                Err(error) => return Err(error),
            }
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
        "sent":sent,"stored":stored,"results":results,"messages":results}),
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
    crate::coordinator::retire_legacy_coordinator(&directory.workspace)?;
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
    let sink = if req.envelope.is_some() {
        crate::messaging::presentation::decide_presentation(req.presentation,
            crate::messaging::presentation::PresentationSource::ReportResult).effective_sink
    } else { req.presentation.sink };
    if stored.duplicate || (req.envelope.is_none() && req.mailbox) || sink != PresentationSink::Leader {
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
            if let Some(action) = directory.recovery_action(recipient, &error) {
                result["action"] = json!(action);
            }
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
