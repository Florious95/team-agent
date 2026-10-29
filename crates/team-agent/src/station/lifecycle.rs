//! C4 target-scoped lifecycle. No coordinator, state.json, or message database.
//! Native adapters own launch/session/CWD details and must verify physical identity.
use super::codec;
use super::seat_store::{component, random_instance, SeatLock, SeatManifest, SeatStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const OPERATION_CAPABILITIES: &[&str] = &["cleanup-before-v1"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Start,
    Stop,
    Restart,
    Reset,
    AddAgent,
    RemoveAgent,
}
impl Action {
    fn runs(self) -> bool {
        matches!(
            self,
            Self::Start | Self::Restart | Self::Reset | Self::AddAgent
        )
    }
    fn replaces(self) -> bool {
        matches!(self, Self::Restart | Self::Reset)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub operation_id: String,
    pub action: Action,
    pub expected_instance: Option<String>,
    /// None retains the seat's existing native launch specification.
    pub launch: Option<Value>,
    pub discard_session: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub v: u8,
    pub operation_id: String,
    pub action: Action,
    pub changed: bool,
    pub manifest: SeatManifest,
}

/// Present is permitted only after instance/binding AND physical birth checks.
/// An ambiguous probe is an error, not Absent. This prevents duplicate spawn on uncertainty.
pub enum Observation {
    Present(Value),
    Absent,
}

pub trait NativeSeat {
    /// Validate only the selected role. Stop/remove must not validate launch configuration.
    fn validate(&mut self, launch: &Value, action: Action) -> Result<(), String>;
    fn observe(&mut self, seat: &SeatManifest) -> Result<Observation, String>;
    /// Recover an already-created instance before spawning a new process. Native
    /// resources must carry the reserved instance binding before they are exposed.
    fn start(
        &mut self,
        seat: &SeatManifest,
        request: &Request,
        previous: Option<&SeatManifest>,
    ) -> Result<Value, String>;
    /// Must recheck identity under the target input lock; absence is success.
    fn stop(&mut self, seat: &SeatManifest) -> Result<(), String>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Operation {
    v: u8,
    request: Request,
    before: Option<SeatManifest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cleanup_before: Vec<SeatManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    after: SeatManifest,
    phase: String,
    changed: bool,
    error: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}
impl Operation {
    fn receipt(&self) -> Receipt {
        Receipt {
            v: 1,
            operation_id: self.request.operation_id.clone(),
            action: self.request.action,
            changed: self.changed,
            manifest: self.after.clone(),
        }
    }
}

pub fn execute(
    store: &SeatStore,
    request: Request,
    native: &mut dyn NativeSeat,
) -> Result<Receipt, String> {
    component(&request.operation_id)?;
    let lock = store.lock()?;
    // A replay returns the recorded result, not a second physical operation.
    if let Some(value) = lock.operation(&request.operation_id)? {
        codec::authorize_mutation(&value, OPERATION_CAPABILITIES)?;
        let mut op: Operation = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if op.request != request {
            return Err("operation_id_conflict".into());
        }
        if op.after.scope != store.scope {
            return Err("operation_scope_mismatch".into());
        }
        return resume(&lock, &mut op, native);
    }

    let mut cleanup_before = Vec::new();
    let mut supersedes = None;
    let mut current = lock.read()?;
    if let Some(seat) = &current {
        validate_manifest(seat)?;
        if request
            .expected_instance
            .as_ref()
            .is_some_and(|id| id != &seat.instance_id)
        {
            return Err("stale_instance".into());
        }
        if matches!(seat.phase.as_str(), "starting" | "stopping") {
            let value = lock
                .operation(&seat.last_operation_id)?
                .ok_or("missing seat operation")?;
            codec::authorize_mutation(&value, OPERATION_CAPABILITIES)?;
            let mut pending: Operation =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            if pending.after.scope != store.scope || pending.after.instance_id != seat.instance_id {
                return Err("operation_scope_mismatch".into());
            }
            validate_operation(&pending, &store.scope)?;
            if !request.action.runs() {
                // Reserve a new stopped/removed instance before cleanup. The
                // interrupted launch can never reclaim authority on replay.
                cleanup_before.extend(pending.before.iter().cloned());
                cleanup_before.extend(pending.cleanup_before.iter().cloned());
                let mut instances = BTreeSet::new();
                cleanup_before.retain(|seat| {
                    seat.desired == "running" && instances.insert(seat.instance_id.clone())
                });
                supersedes = Some(pending.request.operation_id.clone());
            } else {
                let completed = resume(&lock, &mut pending, native)?;
                // A retry of an interrupted restart/reset converges the pending
                // operation, rather than resetting the recovered instance again.
                if same_intent(&request, &pending.request) {
                    let op = Operation {
                        v: 1,
                        request,
                        before: current,
                        cleanup_before: Vec::new(),
                        supersedes: None,
                        after: completed.manifest,
                        phase: "completed".into(),
                        changed: completed.changed,
                        error: None,
                        extra: BTreeMap::new(),
                    };
                    save_operation(&lock, &op)?;
                    return Ok(op.receipt());
                }
                current = Some(completed.manifest);
            }
        }
    } else if request.expected_instance.is_some() {
        return Err("stale_instance".into());
    }

    let launch = request
        .launch
        .clone()
        .or_else(|| current.as_ref().map(|seat| seat.launch.clone()))
        .unwrap_or_else(|| json!({}));
    if let Some(seat) = &current {
        let desired = match request.action {
            Action::RemoveAgent => "removed",
            _ if request.action.runs() => "running",
            _ => "stopped",
        };
        let already_done = if request.action.runs() && !request.action.replaces() {
            seat.desired == "running"
                && seat.phase == "ready"
                && seat.launch == launch
                && matches!(native.observe(seat)?, Observation::Present(_))
        } else {
            !request.action.runs()
                && seat.desired == desired
                && seat.phase == desired
                && matches!(native.observe(seat)?, Observation::Absent)
        };
        if already_done {
            let mut after = seat.clone();
            after.generation = after
                .generation
                .saturating_add(1)
                .min(9_007_199_254_740_991);
            after.last_operation_id = request.operation_id.clone();
            after.updated_at = now();
            let mut op = Operation {
                v: 1,
                request,
                before: current.clone(),
                cleanup_before: Vec::new(),
                supersedes: None,
                after,
                phase: "native_done".into(),
                changed: false,
                error: None,
                extra: BTreeMap::new(),
            };
            save_operation(&lock, &op)?;
            return resume(&lock, &mut op, native);
        }
        if request.action.runs()
            && !request.action.replaces()
            && seat.phase == "ready"
            && seat.launch != launch
        {
            return Err("launch_changed: use restart or reset".into());
        }
    }

    if request.action.runs() {
        native.validate(&launch, request.action)?;
    }
    let mut after = current.clone().unwrap_or_else(|| SeatManifest {
        v: 1,
        kind: "seat_manifest".into(),
        requires: Vec::new(),
        scope: store.scope.clone(),
        instance_id: String::new(),
        generation: 0,
        desired: "stopped".into(),
        phase: "stopped".into(),
        launch: json!({}),
        endpoint: None,
        last_operation_id: String::new(),
        updated_at: String::new(),
        extra: BTreeMap::new(),
    });
    after.instance_id = random_instance()?;
    // Ordering only: never use generation as an authority/admission fence.
    after.generation = after
        .generation
        .saturating_add(1)
        .min(9_007_199_254_740_991);
    after.desired = match request.action {
        Action::RemoveAgent => "removed",
        _ if request.action.runs() => "running",
        _ => "stopped",
    }
    .into();
    after.phase = if request.action.runs() {
        "starting"
    } else {
        "stopping"
    }
    .into();
    after.endpoint = None;
    after.launch = launch;
    after.last_operation_id = request.operation_id.clone();
    after.updated_at = now();
    let mut op = Operation {
        v: 1,
        request,
        before: current,
        cleanup_before,
        supersedes,
        after,
        phase: "reserved".into(),
        changed: true,
        error: None,
        extra: BTreeMap::new(),
    };
    // Intent precedes side effects. A crash before manifest publication is safe
    // to resume only while the previous instance is still current.
    save_operation(&lock, &op)?;
    resume(&lock, &mut op, native)
}

fn resume(
    lock: &SeatLock,
    op: &mut Operation,
    native: &mut dyn NativeSeat,
) -> Result<Receipt, String> {
    validate_operation(op, lock.scope())?;
    if op.phase == "completed" {
        return Ok(op.receipt());
    }
    if op.phase == "cancelled" {
        return Err("operation_cancelled".into());
    }
    let current = lock.read()?;
    if let Some(current) = &current {
        validate_manifest(current)?;
    }
    let same_instance = current
        .as_ref()
        .is_some_and(|seat| seat.instance_id == op.after.instance_id);
    let previous_instance = match (&current, &op.before) {
        (None, None) => true,
        (Some(current), Some(previous)) => current.instance_id == previous.instance_id,
        _ => false,
    };
    if !same_instance && !(op.phase == "reserved" && previous_instance) {
        return Err("stale_operation".into());
    }
    let result: Result<Receipt, String> = (|| {
        if !same_instance || op.phase == "reserved" {
            lock.publish(&op.after)?;
        }
        if op.phase == "reserved" {
            if let Some(id) = &op.supersedes {
                let value = lock.operation(id)?.ok_or("missing superseded operation")?;
                codec::authorize_mutation(&value, OPERATION_CAPABILITIES)?;
                let mut old: Operation =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                validate_operation(&old, lock.scope())?;
                if op
                    .before
                    .as_ref()
                    .is_none_or(|before| before.instance_id != old.after.instance_id)
                {
                    return Err("operation_scope_mismatch".into());
                }
                old.phase = "cancelled".into();
                old.error = Some("superseded by explicit stop/remove".into());
                save_operation(lock, &old)?;
            }
            for before in &op.cleanup_before {
                native.stop(before)?;
            }
            // The new instance is already the authority. Destruction still uses
            // the saved old physical identity, never a reused pane id alone.
            if let Some(before) = &op.before {
                native.stop(before)?;
            }
            op.phase = "old_stopped".into();
            save_operation(lock, op)?;
        }
        if op.phase == "old_stopped" {
            if op.request.action.runs() {
                let endpoint = match native.observe(&op.after)? {
                    Observation::Present(endpoint) => endpoint,
                    Observation::Absent => {
                        native.validate(&op.after.launch, op.request.action)?;
                        native.start(&op.after, &op.request, op.before.as_ref())?
                    }
                };
                op.after.endpoint = Some(endpoint);
                op.after.phase = "ready".into();
            } else {
                op.after.endpoint = None;
                op.after.phase = op.after.desired.clone();
            }
            op.after.updated_at = now();
            op.phase = "native_done".into();
            save_operation(lock, op)?;
        }
        if op.phase == "native_done" {
            if lock
                .read()?
                .as_ref()
                .is_none_or(|current| current.instance_id != op.after.instance_id)
            {
                return Err("stale_instance_after_native_effect".into());
            }
            lock.publish(&op.after)?;
            op.phase = "completed".into();
            op.error = None;
            save_operation(lock, op)?;
        }
        if op.phase != "completed" {
            return Err("unsupported_operation_phase".into());
        }
        Ok(op.receipt())
    })();
    if let Err(error) = &result {
        op.error = Some(error.clone());
        // Preserve the last successful phase; a retry probes instead of guessing.
        let _ = save_operation(lock, op);
    }
    result
}

fn validate_operation(op: &Operation, scope: &super::seat_store::SeatScope) -> Result<(), String> {
    component(&op.request.operation_id)?;
    codec::authorize_mutation(
        &serde_json::to_value(op).map_err(|e| e.to_string())?,
        OPERATION_CAPABILITIES,
    )?;
    if !matches!(op.phase.as_str(), "completed" | "cancelled")
        && op.after.last_operation_id != op.request.operation_id
    {
        return Err("operation_pointer_mismatch".into());
    }
    if op
        .extra
        .get("kind")
        .is_some_and(|kind| kind != "seat_operation")
    {
        return Err("unsupported_operation_kind".into());
    }
    for seat in std::iter::once(&op.after)
        .chain(op.before.iter())
        .chain(op.cleanup_before.iter())
    {
        if &seat.scope != scope {
            return Err("operation_scope_mismatch".into());
        }
        validate_manifest(seat)?;
    }
    Ok(())
}

pub(crate) fn validate_manifest(seat: &SeatManifest) -> Result<(), String> {
    if seat.kind != "seat_manifest" {
        return Err("unsupported_seat_kind".into());
    }
    codec::authorize_mutation(&serde_json::to_value(seat).map_err(|e| e.to_string())?, &[])?;
    if !matches!(seat.desired.as_str(), "running" | "stopped" | "removed")
        || !matches!(
            seat.phase.as_str(),
            "reserved" | "starting" | "ready" | "stopping" | "stopped" | "removed" | "failed"
        )
    {
        return Err("unsupported_seat_state".into());
    }
    if seat.instance_id.len() != 32 || !seat.instance_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid_instance_id".into());
    }
    if seat.phase == "ready" && (seat.desired != "running" || seat.endpoint.is_none()) {
        return Err("invalid_ready_manifest".into());
    }
    Ok(())
}

fn same_intent(left: &Request, right: &Request) -> bool {
    left.action == right.action
        && left.launch == right.launch
        && left.discard_session == right.discard_session
}
fn save_operation(lock: &SeatLock, op: &Operation) -> Result<(), String> {
    let mut value = serde_json::to_value(op).map_err(|e| e.to_string())?;
    value["kind"] = json!("seat_operation");
    if !op.cleanup_before.is_empty() {
        value["requires"] = json!(OPERATION_CAPABILITIES);
    }
    lock.publish_operation(&op.request.operation_id, &value)?;
    lock.prune_operations()
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
