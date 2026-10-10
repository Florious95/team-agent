use std::fs::File;
use std::io::Write;
use std::time::Duration;

use crate::contract::delivery::{
    ControlPasteDiagnostic, DeliveryEffect, InputSurface, StepKind, StepOutcome,
};
use crate::contract::types::{
    AttemptId, Digest, InstanceIdentity, MessageId, Operation, OperationId,
};
use crate::host::files::{FileReceipt, ScopedDirectory};
use crate::host::{digest, digest_hex, HostError, HostErrorKind};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Correlation {
    Business(MessageId),
    NativeControl(OperationId),
    Startup(OperationId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptMetadata {
    pub owner: InstanceIdentity,
    pub attempt: AttemptId,
    pub correlation: Correlation,
    pub operation: Operation,
    pub policy_sha256: Digest,
    pub payload_sha256: Digest,
    pub payload_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalKind {
    ActionIntent,
    ActionResult,
    Surface,
    Accepted,
    Failure,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalRecord {
    pub kind: JournalKind,
    pub ordinal: u64,
    pub at: Duration,
    pub effect: DeliveryEffect,
    pub step: Option<StepKind>,
    pub outcome: Option<StepOutcome>,
    pub sequence: Option<u64>,
    pub surface: Option<InputSurface>,
    pub code: Option<&'static str>,
    pub control_paste: Option<ControlPasteDiagnostic>,
}

pub trait AttemptJournal {
    /// Exclusive create / single attempt. Existing attempts are never replayed here.
    fn begin(&mut self, metadata: &AttemptMetadata) -> Result<(), HostError>;
    fn append(&mut self, record: &JournalRecord) -> Result<(), HostError>;
}

fn owner_json(owner: &InstanceIdentity) -> Value {
    json!({"scope":owner.scope.as_str(),"seat":owner.seat.as_str(),"instance":owner.instance.as_str(),"generation":owner.generation.0})
}

fn metadata_json(metadata: &AttemptMetadata) -> Value {
    let correlation = match &metadata.correlation {
        Correlation::Business(id) => json!({"business":id.as_str()}),
        Correlation::NativeControl(id) => json!({"control":id.as_str()}),
        Correlation::Startup(id) => json!({"startup":id.as_str()}),
    };
    json!({"schema":1,"kind":"begin","owner":owner_json(&metadata.owner),"attempt":metadata.attempt.as_str(),
        "correlation":correlation,"operation":format!("{:?}",metadata.operation),"policy":digest_hex(metadata.policy_sha256),
        "payload_hash":digest_hex(metadata.payload_sha256),"payload_bytes":metadata.payload_bytes})
}

fn record_json(record: &JournalRecord) -> Value {
    let mut value = json!({"schema":1,"kind":format!("{:?}",record.kind),"ordinal":record.ordinal,"at_ns":record.at.as_nanos().to_string(),
        "effect":format!("{:?}",record.effect),"step":record.step.map(|v|format!("{v:?}")),
        "outcome":record.outcome.map(|v|format!("{v:?}")),"sequence":record.sequence,
        "surface":record.surface.map(|v|format!("{v:?}")),"code":record.code});
    if let Some(diagnostic) = record.control_paste {
        value["control_paste"] = json!({
            "after_step": diagnostic.after_step.map(|step| format!("{step:?}")),
            "baseline_has_expected": diagnostic.baseline_has_expected,
            "baseline_composer_empty": diagnostic.baseline_composer_empty,
            "current_composer_has_expected": diagnostic.current_composer_has_expected,
            "fresh": diagnostic.fresh,
            "latch_seen": diagnostic.latch_seen,
            "latch_gone": diagnostic.latch_gone,
            "correlated": diagnostic.correlated,
        });
    }
    value
}

pub fn journal_name(metadata: &AttemptMetadata) -> String {
    let key = format!(
        "{}\0{}\0{}\0{}\0{}",
        metadata.owner.scope.as_str(),
        metadata.owner.seat.as_str(),
        metadata.owner.instance.as_str(),
        metadata.owner.generation.0,
        metadata.attempt.as_str()
    );
    format!("attempt-{}.jsonl", digest_hex(digest(key.as_bytes())))
}

/// JSONL is an owned physical-attempt journal, not a second message/outbox database.
/// Intent and result writes are synced; raw prompt, env, payload and captures are not logged.
pub struct FileJournal {
    directory: ScopedDirectory,
    file: Option<File>,
    created: Option<FileReceipt>,
    bytes: usize,
    max_bytes: usize,
    last_ordinal: Option<u64>,
}

impl FileJournal {
    pub fn new(directory: ScopedDirectory, max_bytes: usize) -> Result<Self, HostError> {
        if max_bytes == 0 {
            return Err(HostError::new("journal limit", HostErrorKind::Invalid));
        }
        Ok(Self {
            directory,
            file: None,
            created: None,
            bytes: 0,
            max_bytes,
            last_ordinal: None,
        })
    }
    /// Initial creation receipt only; an appended journal has changed bytes and cannot be
    /// deleted using this original hash. Its owner must retain/re-observe it explicitly.
    pub fn creation_receipt(&self) -> Option<&FileReceipt> {
        self.created.as_ref()
    }
    pub fn recover(&self, metadata: &AttemptMetadata) -> RecoveryState {
        match self
            .directory
            .read_file(&journal_name(metadata), self.max_bytes)
        {
            Ok(bytes) => recover_bytes(metadata, &bytes),
            Err(error) if error.kind == HostErrorKind::Io(std::io::ErrorKind::NotFound) => {
                RecoveryState::Absent
            }
            Err(_) => RecoveryState::UnknownMayHaveEffect {
                floor: DeliveryEffect::MayHaveSubmitted,
            },
        }
    }
}

impl AttemptJournal for FileJournal {
    fn begin(&mut self, metadata: &AttemptMetadata) -> Result<(), HostError> {
        if self.file.is_some() || metadata.owner != self.directory.receipt().owner {
            return Err(HostError::new(
                "journal owner/attempt",
                HostErrorKind::Ownership,
            ));
        }
        let mut bytes = serde_json::to_vec(&metadata_json(metadata))
            .map_err(|_| HostError::new("journal header", HostErrorKind::Invalid))?;
        bytes.push(b'\n');
        if bytes.len() > self.max_bytes {
            return Err(HostError::new("journal limit", HostErrorKind::Invalid));
        }
        let name = journal_name(metadata);
        match self.directory.create_file(&name, &bytes) {
            Ok(receipt) => self.created = Some(receipt),
            Err(failure) => {
                self.created = failure.possible;
                return Err(failure.error);
            }
        }
        self.bytes = bytes.len();
        self.file = Some(self.directory.open_append(&name)?);
        Ok(())
    }
    fn append(&mut self, record: &JournalRecord) -> Result<(), HostError> {
        self.directory.check_live()?;
        if self.last_ordinal.is_some_and(|last| record.ordinal <= last) {
            return Err(HostError::new("journal ordering", HostErrorKind::Invalid));
        }
        let mut bytes = serde_json::to_vec(&record_json(record))
            .map_err(|_| HostError::new("journal record", HostErrorKind::Invalid))?;
        bytes.push(b'\n');
        if self
            .bytes
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.max_bytes)
        {
            return Err(HostError::new("journal limit", HostErrorKind::Invalid));
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| HostError::new("journal not begun", HostErrorKind::Invalid))?;
        if let Some(receipt) = &mut self.created {
            receipt.effect = crate::contract::plan::ResourceWriteEffect::MayHaveWritten;
        }
        file.write_all(&bytes)
            .and_then(|_| file.sync_data())
            .map_err(|error| HostError::io("journal append", error))?;
        self.bytes += bytes.len();
        self.last_ordinal = Some(record.ordinal);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryState {
    Absent,
    NoInputIntent,
    MayHaveEffect(DeliveryEffect),
    Complete(DeliveryEffect),
    UnknownMayHaveEffect { floor: DeliveryEffect },
}

fn parse_effect(value: &Value) -> Option<DeliveryEffect> {
    match value.as_str()? {
        "NoEffect" => Some(DeliveryEffect::NoEffect),
        "MayHavePasted" => Some(DeliveryEffect::MayHavePasted),
        "PastedUnsubmitted" => Some(DeliveryEffect::PastedUnsubmitted),
        "MayHaveSubmitted" => Some(DeliveryEffect::MayHaveSubmitted),
        "Submitted" => Some(DeliveryEffect::Submitted),
        _ => None,
    }
}

/// Recovery is a classification, never an instruction to paste or replay a key.
/// A torn/malformed/mismatched journal is not repaired into a successful receipt.
pub fn recover_bytes(metadata: &AttemptMetadata, bytes: &[u8]) -> RecoveryState {
    fn unknown(floor: DeliveryEffect) -> RecoveryState {
        RecoveryState::UnknownMayHaveEffect {
            floor: floor.retain_floor(DeliveryEffect::MayHaveSubmitted),
        }
    }
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let Some(header) = lines
        .next()
        .and_then(|line| serde_json::from_slice::<Value>(line).ok())
    else {
        return unknown(DeliveryEffect::NoEffect);
    };
    if header != metadata_json(metadata) {
        return unknown(DeliveryEffect::NoEffect);
    }
    let mut floor = DeliveryEffect::NoEffect;
    let mut ordinal = None;
    let mut complete = false;
    let mut pending: Option<String> = None;
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return unknown(floor);
        };
        let Some(next) = value["ordinal"].as_u64() else {
            return unknown(floor);
        };
        let Some(effect) = parse_effect(&value["effect"]) else {
            return unknown(floor);
        };
        if value["schema"] != 1 || ordinal.is_some_and(|prior| next <= prior) || complete {
            return unknown(floor);
        }
        ordinal = Some(next);
        floor = floor.retain_floor(effect);
        match value["kind"].as_str() {
            Some("ActionIntent") => {
                if pending.is_some() {
                    return unknown(floor);
                }
                let Some(step) = value["step"].as_str() else {
                    return unknown(floor);
                };
                if !matches!(
                    step,
                    "Paste"
                        | "InitialSubmit"
                        | "Confirmation"
                        | "Retry"
                        | "WrapGap"
                        | "QueueFlush"
                        | "StartupAck"
                ) {
                    return unknown(floor);
                }
                pending = Some(step.to_string());
            }
            Some("ActionResult") => {
                if pending.is_none()
                    || pending.as_deref() != value["step"].as_str()
                    || !matches!(
                        value["outcome"].as_str(),
                        Some("Confirmed" | "NoEffect" | "MayHaveOccurred")
                    )
                {
                    return unknown(floor);
                }
                if value["outcome"] != "NoEffect" {
                    floor = floor.retain_floor(if pending.as_deref() == Some("Paste") {
                        DeliveryEffect::MayHavePasted
                    } else {
                        DeliveryEffect::MayHaveSubmitted
                    });
                }
                pending = None;
            }
            Some("Complete") => {
                if pending.is_some() {
                    return unknown(floor);
                }
                complete = true;
            }
            Some("Surface" | "Accepted" | "Failure") => {}
            _ => return unknown(floor),
        }
    }
    if !bytes.ends_with(b"\n") {
        return unknown(floor);
    }
    if let Some(step) = pending {
        return RecoveryState::MayHaveEffect(floor.retain_floor(if step == "Paste" {
            DeliveryEffect::MayHavePasted
        } else {
            DeliveryEffect::MayHaveSubmitted
        }));
    }
    if complete {
        RecoveryState::Complete(floor)
    } else if floor != DeliveryEffect::NoEffect {
        RecoveryState::MayHaveEffect(floor)
    } else {
        RecoveryState::NoInputIntent
    }
}
