//! Append-only C3 segments. One recipient lock; no SQL, message index or status table.
use crate::station::codec;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const DEDUPE_WINDOW: usize = 1024;
const SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum RecordKind {
    Message,
    ReportResult,
    Unknown(String),
}
impl From<String> for RecordKind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "message" => Self::Message,
            "result" => Self::ReportResult,
            _ => Self::Unknown(s),
        }
    }
}
impl From<RecordKind> for String {
    fn from(s: RecordKind) -> Self {
        match s {
            RecordKind::Message => "message".into(),
            RecordKind::ReportResult => "result".into(),
            RecordKind::Unknown(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    pub segment: u64,
    pub seq: u64,
    pub offset: u64,
    pub length: u64,
    pub checksum: String,
}
impl RecordRef {
    pub fn token(&self) -> String {
        format!(
            "ss1:{}:{}:{}:{}:{}",
            self.segment, self.seq, self.offset, self.length, self.checksum
        )
    }
    pub fn parse(token: &str) -> Result<Self, String> {
        let p: Vec<_> = token.split(':').collect();
        if p.len() != 6
            || p[0] != "ss1"
            || p[5].len() != 64
            || !p[5].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid_dedupe_key".into());
        }
        let number = |i: usize| {
            p[i].parse::<u64>()
                .map_err(|_| "invalid_dedupe_key".to_string())
        };
        let value = Self {
            segment: number(1)?,
            seq: number(2)?,
            offset: number(3)?,
            length: number(4)?,
            checksum: p[5].to_string(),
        };
        if value.segment == 0 || value.seq == 0 || value.length == 0 {
            return Err("invalid_dedupe_key".into());
        }
        Ok(value)
    }
}
pub struct StoredRecord {
    pub record: Value,
    pub reference: RecordRef,
    pub duplicate: bool,
}

fn component(s: &str) -> Result<&str, String> {
    if s.is_empty()
        || matches!(s, "." | "..")
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("invalid inbox scope component".into());
    }
    Ok(s)
}
fn root(workspace: &Path, recipient: &str, team: Option<&str>) -> Result<PathBuf, String> {
    let mut root = workspace.join(".team/inbox");
    if let Some(team) = team {
        root.push(component(team)?);
    }
    Ok(root.join(component(recipient)?))
}
pub(crate) fn task_path(
    workspace: &Path,
    recipient: &str,
    team: Option<&str>,
) -> Result<PathBuf, String> {
    Ok(root(workspace, recipient, team)?
        .parent()
        .unwrap()
        .join(format!("{recipient}.context.json")))
}
fn mkdir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let missing: Vec<_> = path
        .ancestors()
        .take_while(|p| !p.exists())
        .map(Path::to_path_buf)
        .collect();
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())?;
    // A durable segment is insufficient if a newly-created ancestor entry can
    // disappear on reboot. Persist each new directory's link before accepting data.
    #[cfg(unix)]
    for directory in missing.iter().rev() {
        File::open(directory.parent().ok_or("directory has no parent")?)
            .and_then(|parent| parent.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn open(path: &Path, create: bool) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).append(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|e| e.to_string())
}
fn segments(root: &Path) -> Result<Vec<(u64, PathBuf)>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            if let Some(n) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_suffix(".jsonl"))
                .and_then(|s| s.parse::<u64>().ok())
            {
                paths.push((n, entry.path()));
            }
        }
    }
    paths.sort_by_key(|(n, _)| *n);
    Ok(paths)
}
fn segment_path(root: &Path, n: u64) -> PathBuf {
    root.join(format!("{n:08}.jsonl"))
}

/// Reverse bounded scanning: at most one record-sized carry buffer, never a
/// whole history allocation. A non-newline-terminated suffix is not committed.
fn reverse_lines(
    path: &Path,
    mut visit: impl FnMut(u64, &[u8]) -> Result<bool, String>,
) -> Result<bool, String> {
    let mut file = open(path, false)?;
    let mut end = file.metadata().map_err(|e| e.to_string())?.len();
    let mut carry = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(8192);
        let mut bytes = vec![0; (end - start) as usize];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        bytes.extend_from_slice(&carry);
        let Some(mut right) = bytes.iter().rposition(|b| *b == b'\n').map(|n| n + 1) else {
            end = start;
            continue;
        };
        loop {
            let left = bytes[..right - 1]
                .iter()
                .rposition(|b| *b == b'\n')
                .map(|n| n + 1);
            match left {
                Some(left) => {
                    if !visit(start + left as u64, &bytes[left..right])? {
                        return Ok(false);
                    }
                    right = left;
                }
                None if start == 0 => {
                    if !visit(0, &bytes[..right])? {
                        return Ok(false);
                    }
                    carry.clear();
                    break;
                }
                None => {
                    carry = bytes[..right].to_vec();
                    break;
                }
            }
        }
        end = start;
    }
    Ok(true)
}
fn scan_recent(
    paths: &[(u64, PathBuf)],
    limit: usize,
    mut visit: impl FnMut(Value, RecordRef) -> Result<bool, String>,
) -> Result<usize, String> {
    let mut seen = 0;
    let mut malformed = 0;
    for (segment, path) in paths.iter().rev() {
        if !reverse_lines(path, |offset, line| {
            if line.iter().all(u8::is_ascii_whitespace) {
                return Ok(true);
            }
            let value = match codec::decode(line) {
                Ok(v) => v,
                Err(_) => {
                    malformed += 1;
                    return Ok(true);
                }
            };
            let reference = RecordRef {
                segment: *segment,
                seq: value["seq"].as_u64().unwrap_or(0),
                offset,
                length: line.len() as u64,
                checksum: value["checksum"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{:x}", Sha256::digest(line))),
            };
            seen += 1;
            Ok(visit(value, reference)? && seen < limit)
        })? {
            break;
        }
    }
    Ok(malformed)
}
fn same_request(a: &Value, b: &Value) -> bool {
    let normalize = |v: &Value| {
        let mut v = v.clone();
        if let Some(fields) = v.as_object_mut() {
            for key in [
                "v",
                "requires",
                "message_id",
                "request_id",
                "created_at",
                "seq",
                "checksum",
                "provenance",
                "dedupe_key",
                "record_ref",
            ] {
                fields.remove(key);
            }
            if let Some(envelope) = fields.get_mut("envelope").and_then(Value::as_object_mut) {
                envelope.remove("dedupe_key");
                envelope.remove("record_ref");
            }
        }
        v.sort_all_objects();
        v
    };
    normalize(a) == normalize(b)
}
fn key(record: &Value) -> Option<&str> {
    record
        .get("request_id")
        .or_else(|| record.get("message_id"))
        .and_then(Value::as_str)
}
fn referenced(root: &Path, reference: &RecordRef) -> Result<Value, String> {
    let mut file = open(&segment_path(root, reference.segment), false)
        .map_err(|_| "idempotency_window_expired")?;
    if reference
        .offset
        .checked_add(reference.length)
        .is_none_or(|n| n > file.metadata().map(|m| m.len()).unwrap_or(0))
    {
        return Err("idempotency_window_expired".into());
    }
    file.seek(SeekFrom::Start(reference.offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(reference.length)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let value = codec::decode(&bytes)?;
    if value["seq"].as_u64() != Some(reference.seq)
        || value["checksum"].as_str() != Some(&reference.checksum)
    {
        return Err("dedupe_reference_mismatch".into());
    }
    Ok(value)
}

pub fn append(workspace: &Path, recipient: &str, record: &Value) -> Result<(), String> {
    if record.get("v").is_some() {
        return store(workspace, recipient, record).map(|_| ());
    }
    // Explicit unversioned compatibility path; new business ingress always emits v1.
    let root = root(workspace, recipient, record["team"].as_str())?;
    mkdir(root.parent().unwrap())?;
    let mut file = open(
        &root.parent().unwrap().join(format!("{recipient}.jsonl")),
        true,
    )?;
    file.lock().map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())
}
pub fn store(workspace: &Path, recipient: &str, record: &Value) -> Result<StoredRecord, String> {
    if record.get("v").is_some_and(|v| v.as_u64() != Some(1)) {
        return Err("unsupported_version".into());
    }
    if record.get("v").is_some() {
        codec::authorize_mutation(record, &["station.inbox-commit.v1"])?;
    }
    let root = root(workspace, recipient, record["team"].as_str())?;
    mkdir(&root)?;
    let locks = root.parent().unwrap().join(".locks");
    mkdir(&locks)?;
    let lock = open(&locks.join(format!("{recipient}.lock")), true)?;
    lock.lock().map_err(|e| e.to_string())?; // flock on Unix; LockFileEx on Windows
    if let Some(token) = record["dedupe_key"].as_str() {
        let reference = RecordRef::parse(token)?;
        let existing = referenced(&root, &reference)?;
        if !same_request(&existing, record) {
            return Err("dedupe_key_conflict".into());
        }
        return Ok(StoredRecord {
            record: existing,
            reference,
            duplicate: true,
        });
    }
    let paths = segments(&root)?;
    let mut last = 0;
    let mut duplicate = None;
    scan_recent(&paths, DEDUPE_WINDOW, |existing, reference| {
        last = last.max(reference.seq);
        if key(record).is_some() && key(&existing) == key(record) && existing["opaque"] != true {
            if !same_request(&existing, record) {
                return Err("request_id_conflict".into());
            }
            duplicate = Some(StoredRecord {
                record: existing,
                reference,
                duplicate: true,
            });
            return Ok(false);
        }
        Ok(true)
    })?;
    if let Some(record) = duplicate {
        return Ok(record);
    }
    let seq = last.checked_add(1).ok_or("sequence exhausted")?;
    let line = codec::encode(record, seq)?;
    let mut segment = paths.last().map_or(1, |(n, _)| *n);
    if let Some((_, path)) = paths.last() {
        let length = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if length > 0 && length.saturating_add(line.len() as u64) > SEGMENT_BYTES {
            segment = segment.checked_add(1).ok_or("segment exhausted")?;
        }
    }
    let path = segment_path(&root, segment);
    let mut file = open(&path, true)?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length > 0 {
        file.seek(SeekFrom::End(-1)).map_err(|e| e.to_string())?;
        let mut last = [0];
        file.read_exact(&mut last).map_err(|e| e.to_string())?;
        if last[0] != b'\n' {
            file.write_all(b"\n").map_err(|e| e.to_string())?;
        }
    }
    let offset = file.metadata().map_err(|e| e.to_string())?.len();
    file.write_all(&line).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    File::open(&root)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;
    let record = codec::decode(&line)?;
    let reference = RecordRef {
        segment,
        seq,
        offset,
        length: line.len() as u64,
        checksum: record["checksum"]
            .as_str()
            .ok_or("checksum missing")?
            .into(),
    };
    Ok(StoredRecord {
        record,
        reference,
        duplicate: false,
    })
}

pub fn record(
    sender: &str,
    recipient: &str,
    content: &str,
    team: Option<&str>,
    kind: &str,
) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "inbox-{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    json!({"v":1,"message_id":id,"created_at":chrono::Utc::now().to_rfc3339(),"sender":sender,"recipient":recipient,"content":content,"team":team,"kind":kind})
}
/// No acknowledgment, repair, migration, or filesystem creation on read.
pub fn read(
    workspace: &Path,
    recipient: &str,
    limit: usize,
    team: Option<&str>,
) -> Result<Value, String> {
    let selected = team.map(str::to_string).or_else(|| {
        crate::direct_delivery::directory(workspace, None)
            .ok()
            .and_then(|d| d.team)
    });
    let root = root(workspace, recipient, selected.as_deref())?;
    let paths = segments(&root)?;
    let scoped_legacy = root.parent().unwrap().join(format!("{recipient}.jsonl"));
    let global_legacy = workspace.join(".team/inbox").join(format!("{recipient}.jsonl"));
    let mut messages = VecDeque::new();
    let mut malformed = if limit == 0 {
        0
    } else {
        scan_recent(&paths, limit, |value, _| {
            if value["opaque"] == true
                || selected
                    .as_deref()
                    .is_none_or(|team| value["team"].as_str() == Some(team))
            {
                messages.push_front(value);
            }
            Ok(true)
        })?
    };
    let mut previous = None;
    for value in &messages {
        if value["opaque"] == true {
            previous = None;
            continue;
        }
        if let Some(seq) = value["seq"].as_u64() {
            if previous.is_some_and(|prev| seq != prev + 1) {
                return Err(format!(
                    "seq_gap: expected {}, found {seq}",
                    previous.unwrap() + 1
                ));
            }
            previous = Some(seq);
        }
    }
    // Old helpers can keep writing either flat file after the new segments exist.
    // Filter before limiting each legacy stream; never assign unscoped rows in a
    // multi-team workspace merely because the caller selected one team.
    let single_team = crate::direct_delivery::snapshot(workspace).ok().is_some_and(|state| {
        let matches = |key: &str, team: &Value| selected.as_deref().is_none_or(|selected|
            crate::state::projection::team_selector_matches(selected, key, team));
        match state.get("teams").and_then(Value::as_object).filter(|teams| !teams.is_empty()) {
            Some(teams) => teams.len() == 1 && teams.iter().any(|(key, team)| matches(key, team)),
            None => ["team_key", "active_team_key", "team_id"].iter()
                .filter_map(|key| state.get(key).and_then(Value::as_str))
                .any(|key| !key.is_empty() && matches(key, &state)),
        }
    });
    let mut legacy_paths = vec![scoped_legacy];
    if legacy_paths[0] != global_legacy { legacy_paths.push(global_legacy); }
    let mut unattributed = 0;
    let mut merged_legacy = false;
    for path in legacy_paths.into_iter().filter(|path| path.is_file()) {
        if limit == 0 { break; }
        let mut tail = VecDeque::new();
        malformed += scan_recent(&[(0, path)], usize::MAX, |record, _| {
            let record_team = record.get("team").and_then(Value::as_str);
            if record["opaque"] == true || (!single_team && (record_team.is_none() || selected.is_none())) {
                unattributed += 1;
                return Ok(true);
            }
            if selected.as_deref().is_none_or(|team| record_team.is_none_or(|owner| owner == team)) {
                tail.push_front(record);
            }
            Ok(tail.len() < limit)
        })?;
        for record in tail {
            let duplicate = record.get("message_id").and_then(Value::as_str).is_some_and(|id|
                messages.iter().any(|message| message.get("message_id").and_then(Value::as_str) == Some(id)));
            if !duplicate { messages.push_back(record); merged_legacy = true; }
        }
    }
    if merged_legacy {
        messages.make_contiguous().sort_by_cached_key(|message|
            message.get("created_at").and_then(Value::as_str)
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()));
    }
    while messages.len() > limit { messages.pop_front(); }
    let mut value = json!({"recipient":recipient,"messages":messages});
    if unattributed > 0 { value["unattributed_legacy_records"] = json!(unattributed); }
    if malformed > 0 {
        value["malformed_records"] = json!(malformed);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TempWorkspace(PathBuf);
    impl TempWorkspace {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{label}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_missing_inbox_is_side_effect_free() {
        let ws = TempWorkspace::new("direct-inbox-missing");
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            json!([])
        );
        assert!(!ws.path().join(".team/inbox").exists());
        assert!(read(ws.path(), "../escape", 3, None).is_err());
    }

    #[test]
    fn tails_records_and_does_not_consume_them() {
        let ws = TempWorkspace::new("direct-inbox-tail");
        for n in 0..5 {
            append(ws.path(), "leader", &json!({"content": n})).unwrap();
        }
        let expected = json!([{"content": 2}, {"content": 3}, {"content": 4}]);
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            expected
        );
        assert_eq!(
            read(ws.path(), "leader", 3, None).unwrap()["messages"],
            expected
        );
    }
}
