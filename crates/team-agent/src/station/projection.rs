//! Compatibility projection of authoritative per-seat C4 records.
//! Only lifecycle publishers write these records. Readers never repair state.json.
use super::codec;
use super::seat_store::{self, SeatManifest, SeatScope, SeatStore};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

const INSTANCE: &str = "station_instance_id";

fn binding(workspace: &Path, create: bool) -> Result<Option<String>, String> {
    let canonical = workspace.canonicalize().map_err(|e| e.to_string())?;
    let root = canonical.join(".team/station-v1");
    let path = root.join("workspace.json");
    if !path.exists() && create {
        seat_store::private_dir(&root)?;
        let id = seat_store::random_instance()?;
        let temp = root.join(format!(".workspace-{id}.json"));
        let value = json!({"v":1,"kind":"workspace","workspace_id":id,"canonical_path":canonical});
        seat_store::write_json(&temp, &value, 16 * 1024)?;
        let installed = fs::hard_link(&temp, &path);
        let _ = fs::remove_file(&temp);
        match installed {
            Ok(()) => seat_store::sync_directory(&root)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    let text = match fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let value = codec::read(&text)?;
    codec::authorize_mutation(&value, &[])?;
    if value["canonical_path"].as_str() != canonical.to_str() {
        return Err("workspace_copy_requires_rebind".into());
    }
    value["workspace_id"].as_str().map(|s| Some(s.to_string())).ok_or("invalid workspace binding".into())
}

/// Publish only seats for which the lifecycle caller has write authority. Peer
/// health/capture/coordinator writers must not call this API.
pub(crate) fn publish(
    workspace: &Path,
    state: &mut Value,
    team: &str,
    ids: &[&str],
) -> Result<(), String> {
    seat_store::component(team)?;
    let workspace_id = binding(workspace, true)?.ok_or("workspace identity missing")?;
    let mut context = state.clone();
    let object = context.as_object_mut().ok_or("invalid team projection")?;
    for field in ["agents", "teams", "agent_lifecycle", "tasks", "agent_health"] {
        object.remove(field);
    }
    object.insert("team_key".into(), json!(team));
    object.insert("active_team_key".into(), json!(team));
    for id in ids {
        let store = SeatStore::new(workspace, SeatScope {
            workspace: workspace_id.clone(), team: team.into(), seat: (*id).into(),
        })?;
        let lock = store.lock()?;
        let previous = lock.read()?;
        if let Some(previous) = &previous {
            codec::authorize_mutation(&serde_json::to_value(previous).map_err(|e| e.to_string())?, &[])?;
        }
        let row = state.get("agents").and_then(|agents| agents.get(*id)).cloned();
        let Some(mut row) = row else {
            if let Some(mut seat) = previous {
                seat.instance_id = seat_store::random_instance()?;
                seat.generation = seat.generation.saturating_add(1);
                seat.desired = "removed".into();
                seat.phase = "removed".into();
                seat.endpoint = None;
                seat.updated_at = now();
                seat.extra.insert("legacy_context".into(), context.clone());
                lock.publish(&seat)?;
            }
            continue;
        };
        if let Some(previous) = &previous {
            if let Some(expected) = row.get(INSTANCE).and_then(Value::as_str) {
                if expected != previous.instance_id {
                    // A later seat operation already won. Never roll it back
                    // with an aggregate caller's older projection snapshot.
                    if let Some(current) = previous.extra.get("legacy_row") {
                        state["agents"][*id] = current.clone();
                    }
                    continue;
                }
            }
        }
        let explicitly_stopped = row.get("explicit_stop").and_then(Value::as_bool) == Some(true);
        let desired = if explicitly_stopped { "stopped" } else { "running" };
        let status = row.get("status").and_then(Value::as_str).unwrap_or("stopped");
        let phase = match status {
            "running" => "ready", "starting" => "starting", "removed" => "removed",
            "failed" => "failed", _ => "stopped",
        };
        let old_row = previous.as_ref().and_then(|seat| seat.extra.get("legacy_row"));
        let changed_cohort = old_row.is_some_and(|old| {
            ["pane_id", "pane_pid", "spawned_at"].iter().any(|key| old.get(*key) != row.get(*key))
        });
        let change_instance = previous.as_ref().is_none_or(|seat| seat.desired != desired || changed_cohort);
        let instance = if change_instance { seat_store::random_instance()? } else {
            previous.as_ref().ok_or("missing previous seat")?.instance_id.clone()
        };
        row[INSTANCE] = json!(instance);
        let endpoint = if phase == "ready" {
            Some(json!({"pane_id":row.get("pane_id"),"pane_pid":row.get("pane_pid"),
                "session_name":context.get("session_name"),"tmux_endpoint":context.get("tmux_endpoint"),
                "binding":instance}))
        } else { None };
        let mut extra = previous.as_ref().map(|seat| seat.extra.clone()).unwrap_or_default();
        extra.insert("legacy_context".into(), context.clone());
        extra.insert("legacy_row".into(), row.clone());
        let seat = SeatManifest {
            v: 1, kind: "seat_manifest".into(), requires: Vec::new(), scope: store.scope.clone(),
            instance_id: instance, generation: previous.as_ref().map_or(1, |seat|
                seat.generation.saturating_add(u64::from(change_instance))),
            desired: desired.into(), phase: phase.into(), launch: row.clone(), endpoint,
            last_operation_id: previous.as_ref().map(|seat| seat.last_operation_id.clone()).unwrap_or_default(),
            updated_at: now(), extra,
        };
        lock.publish(&seat)?;
        state["agents"][*id] = row;
    }
    Ok(())
}

/// Reconstruct a read-only compatibility view from seat records. This is also
/// used after a torn projection: raw corrupt state.json is left untouched.
pub(crate) fn read_workspace(workspace: &Path) -> Result<Option<Value>, String> {
    let Some(workspace_id) = binding(workspace, false)? else { return Ok(None); };
    let root = workspace.join(".team/station-v1");
    let mut teams = Map::new();
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() { continue; }
        let team = entry.file_name().to_string_lossy().into_owned();
        let seats = entry.path().join("seats");
        let entries = match fs::read_dir(seats) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        let mut view: Option<Value> = None;
        let mut agents = Map::new();
        let mut retired = Map::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue; };
            let store = SeatStore::new(workspace, SeatScope {
                workspace: workspace_id.clone(), team: team.clone(), seat: id.into(),
            })?;
            let Some(seat) = store.read()? else { continue; };
            if view.is_none() { view = seat.extra.get("legacy_context").cloned(); }
            if seat.desired == "removed" {
                retired.insert(id.into(), json!({"state":"retired"}));
                continue;
            }
            if let Some(row) = seat.extra.get("legacy_row") {
                let mut row = row.clone();
                row[INSTANCE] = json!(seat.instance_id);
                row["explicit_stop"] = json!(seat.desired == "stopped");
                agents.insert(id.into(), row);
            }
        }
        if let Some(mut view) = view {
            view["agents"] = Value::Object(agents);
            view["agent_lifecycle"] = Value::Object(retired);
            teams.insert(team, view);
        }
    }
    if teams.is_empty() { return Ok(None); }
    let mut result = if teams.len() == 1 {
        let (key, view) = teams.iter().next().ok_or("missing team")?;
        let mut view = view.clone();
        view["active_team_key"] = json!(key);
        view
    } else {
        // Recovery cannot infer which team a user intended from directory order.
        json!({"agents":{},"active_team_key":null})
    };
    result["teams"] = Value::Object(teams);
    result["station_projection"] = json!(true);
    Ok(Some(result))
}

fn now() -> String { chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) }
