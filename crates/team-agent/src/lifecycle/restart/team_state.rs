use super::*;

pub(crate) fn write_team_state(
    workspace: &Path,
    spec: &YamlValue,
    state: &serde_json::Value,
) -> Result<std::path::PathBuf, LifecycleError> {
    let rel = spec
        .get("context")
        .and_then(|v| v.get("state_file"))
        .and_then(YamlValue::as_str)
        .unwrap_or("team_state.md");
    let path = workspace.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("write team_state: {e}")))?;
    }
    // golden state.py:625-686 — byte-faithful layout (probed). Blank line after the title and after each
    // '##' header; '## Team' / '## Latest Results' sections; Task Graph as
    // `- {id} [{status}], assignee={assignee|unassigned}, deps={deps|none}: {title}`.
    let mut lines: Vec<String> = Vec::new();
    lines.push("# Team State".to_string());
    lines.push(String::new());
    lines.push(format!("Updated: {}", team_state_now()));
    lines.push(String::new());
    lines.push("## Objective".to_string());
    lines.push(String::new());
    lines.push(
        spec.get("team")
            .and_then(|v| v.get("objective"))
            .and_then(YamlValue::as_str)
            .unwrap_or("")
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## Team".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- Name: {}",
        spec.get("team")
            .and_then(|v| v.get("name"))
            .and_then(YamlValue::as_str)
            .unwrap_or("None")
    ));
    lines.push(format!(
        "- Runtime session: {}",
        state
            .get("session_name")
            .and_then(|v| v.as_str())
            .unwrap_or("None")
    ));
    // leader_receiver (golden state.py:642-651): direct_tmux line, else inbox-fallback + log lines.
    if let Some(receiver) = state
        .get("leader_receiver")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty())
    {
        let g = |k: &str| receiver.get(k).and_then(|v| v.as_str()).unwrap_or("None");
        if receiver.get("mode").and_then(|v| v.as_str()) == Some("direct_tmux") {
            lines.push(format!(
                "- Leader receiver: direct tmux {} ({}, {})",
                g("pane_id"),
                g("provider"),
                g("status")
            ));
        } else {
            lines.push(format!(
                "- Leader inbox fallback: {}:{} ({})",
                g("session"),
                g("window"),
                g("status")
            ));
            lines.push(format!("- Leader inbox log: {}", g("path")));
        }
    }
    lines.push(String::new());
    lines.push("## Agents".to_string());
    lines.push(String::new());
    if let Some(agents) = spec.get("agents").and_then(YamlValue::as_list) {
        for agent in agents {
            let Some(id) = agent.get("id").and_then(YamlValue::as_str) else {
                continue;
            };
            let role = agent.get("role").and_then(YamlValue::as_str).unwrap_or(id);
            let provider = agent
                .get("provider")
                .and_then(YamlValue::as_str)
                .unwrap_or("codex");
            let status = state
                .get("agents")
                .and_then(|v| v.get(id))
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            lines.push(format!("- {id}: {role} on {provider} ({status})"));
        }
    }
    lines.push(String::new());
    lines.push("## Task Graph".to_string());
    lines.push(String::new());
    // golden: runtime.get("tasks", spec.get("tasks", [])) — state tasks else spec tasks.
    let tasks = team_state_tasks(spec, state);
    for task in &tasks {
        let id = task_field_str(task, "id");
        let title = task_field_str(task, "title");
        let status = {
            let s = task_field_str(task, "status");
            if s.is_empty() {
                "pending".to_string()
            } else {
                s
            }
        };
        let assignee = {
            let a = task_field_str(task, "assignee");
            if a.is_empty() {
                "unassigned".to_string()
            } else {
                a
            }
        };
        let deps = {
            let d = task_field_list(task, "deps");
            if d.is_empty() {
                "none".to_string()
            } else {
                d.join(", ")
            }
        };
        lines.push(format!(
            "- {id} [{status}], assignee={assignee}, deps={deps}: {title}"
        ));
        let summary = task_field_str(task, "last_result_summary");
        if !summary.is_empty() {
            lines.push(format!("  Summary: {summary}"));
        }
        for art in task_artifact_refs(task) {
            lines.push(art);
        }
    }
    lines.push(String::new());
    lines.push("## Latest Results".to_string());
    lines.push(String::new());
    // remove/stop/reset pass no results -> header + blank only (golden).
    lines.push(String::new());
    lines.push("## Blockers".to_string());
    lines.push(String::new());
    let blockers: Vec<&TeamStateTask> = tasks
        .iter()
        .filter(|t| {
            matches!(
                task_field_str(t, "status").as_str(),
                "blocked" | "failed" | "needs_retry"
            )
        })
        .collect();
    if blockers.is_empty() {
        lines.push("- None".to_string());
    } else {
        for task in blockers {
            let id = task_field_str(task, "id");
            let summary = {
                let s = task_field_str(task, "last_result_summary");
                if s.is_empty() {
                    task_field_str(task, "title")
                } else {
                    s
                }
            };
            lines.push(format!("- {id}: {summary}"));
        }
    }
    if let Some(notes) = team_state_notes(state).filter(|notes| !notes.is_empty()) {
        lines.push(String::new());
        lines.push("## Notes".to_string());
        lines.push(String::new());
        for note in notes {
            lines.push(format!("- {note}"));
        }
    }
    lines.push(String::new());
    lines.push("## Next Step".to_string());
    lines.push(String::new());
    lines.push("- Continue routing ready tasks and collect result envelopes.".to_string());
    let text = format!("{}\n", lines.join("\n"));
    std::fs::write(&path, text)
        .map_err(|e| LifecycleError::StatePersist(format!("write team_state: {e}")))?;
    Ok(path)
}

/// A task row sourced from either runtime state (serde_json) or the spec (YAML), so write_team_state
/// can mirror golden's `runtime.get("tasks", spec.get("tasks", []))` fallback.
enum TeamStateTask {
    Json(serde_json::Value),
    Yaml(YamlValue),
}

/// golden `datetime.now(timezone.utc).isoformat()` analog (microseconds + `+00:00`).
fn team_state_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

fn team_state_tasks(spec: &YamlValue, state: &serde_json::Value) -> Vec<TeamStateTask> {
    if let Some(tasks) = state.get("tasks").and_then(|v| v.as_array()) {
        return tasks.iter().cloned().map(TeamStateTask::Json).collect();
    }
    if let Some(tasks) = spec.get("tasks").and_then(YamlValue::as_list) {
        return tasks.iter().cloned().map(TeamStateTask::Yaml).collect();
    }
    Vec::new()
}

fn team_state_notes(state: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        state
            .get("notes")?
            .as_array()?
            .iter()
            .filter_map(|note| {
                note.as_str()
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
            })
            .collect(),
    )
}

fn task_field_str(task: &TeamStateTask, key: &str) -> String {
    match task {
        TeamStateTask::Json(v) => v
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        TeamStateTask::Yaml(v) => v
            .get(key)
            .and_then(YamlValue::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn task_field_list(task: &TeamStateTask, key: &str) -> Vec<String> {
    match task {
        TeamStateTask::Json(v) => v
            .get(key)
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        TeamStateTask::Yaml(v) => v
            .get(key)
            .and_then(YamlValue::as_list)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn task_artifact_refs(task: &TeamStateTask) -> Vec<String> {
    match task {
        TeamStateTask::Json(v) => v
            .get("artifact_refs")
            .and_then(|v| v.as_array())
            .map(|refs| {
                refs.iter()
                    .map(|r| match r.as_object() {
                        Some(m) => format!(
                            "  Artifact: {} - {}",
                            m.get("path").and_then(|v| v.as_str()).unwrap_or("None"),
                            m.get("description").and_then(|v| v.as_str()).unwrap_or("")
                        ),
                        None => format!("  Artifact: INVALID artifact ref {r}"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        TeamStateTask::Yaml(v) => v
            .get("artifact_refs")
            .and_then(YamlValue::as_list)
            .map(|refs| {
                refs.iter()
                    .map(|r| match r.as_map() {
                        Some(_) => format!(
                            "  Artifact: {} - {}",
                            r.get("path").and_then(YamlValue::as_str).unwrap_or("None"),
                            r.get("description")
                                .and_then(YamlValue::as_str)
                                .unwrap_or("")
                        ),
                        None => format!("  Artifact: INVALID artifact ref {:?}", r),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}
