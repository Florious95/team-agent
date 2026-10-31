//! C5: durable, team-scoped jobs. Readers never create files or open SQLite.
//! The runner owns only outstanding work and exits after sixty idle seconds.
mod runtime;
#[cfg(unix)]
mod wait;
mod supervisor;

use super::{codec, seat_store};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

pub use runtime::run;
const CAPABILITIES: &[&str] = &["agenda-reminder-v1"];
const MAX_JOB_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub v: u8,
    pub requires: Vec<String>,
    pub id: String,
    pub workspace: String,
    pub team: String,
    pub text: String,
    pub trigger: String,
    pub created_at: String,
    pub due_at: String,
    pub interval_ms: Option<i64>,
    pub occurrence: u64,
    pub status: String,
    pub result: Option<Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}
impl Job {
    fn due(&self) -> Result<DateTime<Utc>, String> {
        DateTime::parse_from_rfc3339(&self.due_at)
            .map(|time| time.with_timezone(&Utc))
            .map_err(|e| e.to_string())
    }
    fn view(&self) -> Value {
        json!({"id":self.id,"trigger":self.trigger,"text":self.text,
            "status":self.status,"due_at":self.due_at,"occurrence":self.occurrence,
            "result":self.result})
    }
}

#[derive(Clone)]
pub struct Store {
    pub(crate) workspace: PathBuf,
    pub(crate) team: String,
    pub(crate) root: PathBuf,
}
impl Store {
    pub fn new(workspace: &Path, team: &str) -> Result<Self, String> {
        seat_store::component(team)?;
        let workspace = fs::canonicalize(workspace).map_err(|e| e.to_string())?;
        let root = workspace.join(".team/agenda").join(team);
        Ok(Self {
            workspace,
            team: team.into(),
            root,
        })
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        seat_store::component(id)?;
        if !id.starts_with("sched_") {
            return Err("invalid schedule id".into());
        }
        Ok(self.root.join("jobs").join(format!("{id}.json")))
    }
    fn read(&self, id: &str) -> Result<Option<Job>, String> {
        self.read_path(id, &self.path(id)?)
    }
    fn read_path(&self, id: &str, path: &Path) -> Result<Option<Job>, String> {
        let Some(value) = seat_store::read_json(path, MAX_JOB_BYTES)? else {
            return Ok(None);
        };
        codec::authorize_mutation(&value, CAPABILITIES)?;
        let job: Job = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if job.id != id || job.team != self.team || Path::new(&job.workspace) != self.workspace {
            return Err("agenda_scope_mismatch".into());
        }
        if !matches!(
            job.status.as_str(),
            "active" | "running" | "done" | "cancelled" | "failed"
        ) {
            return Err("unsupported agenda status".into());
        }
        if job.text.trim().is_empty() || job.text.len() > 64 * 1024 {
            return Err("invalid reminder text".into());
        }
        if job
            .interval_ms
            .is_some_and(|ms| ms <= 0 || Duration::try_milliseconds(ms).is_none())
        {
            return Err("invalid recurring interval".into());
        }
        job.due()?;
        Ok(Some(job))
    }
    fn write(&self, job: &Job) -> Result<(), String> {
        seat_store::write_json(
            &self.path(&job.id)?,
            &serde_json::to_value(job).map_err(|e| e.to_string())?,
            MAX_JOB_BYTES,
        )
    }
    pub(crate) fn lock(&self, key: &str) -> Result<Lock, String> {
        seat_store::component(key)?;
        let path = if key == "activation" {
            supervisor::activation_lock_path(self)?
        } else {
            let dir = self.root.join("locks");
            seat_store::private_dir(&dir)?;
            dir.join(format!("{key}.lock"))
        };
        let file = seat_store::open_file(&path, false)?;
        let deadline = std::time::Instant::now() + StdDuration::from_secs(10);
        loop {
            match crate::platform::file_lock::try_lock_once_nonblocking(&file)
                .map_err(|e| e.to_string())?
            {
                true => return Ok(Lock(file)),
                false if key == "runner" || std::time::Instant::now() >= deadline => {
                    return Err("agenda_lock_busy".into())
                }
                false => std::thread::sleep(StdDuration::from_millis(10)),
            }
        }
    }
    pub(crate) fn try_activation_lock(&self) -> Result<Option<Lock>, String> {
        let file = seat_store::open_file(&supervisor::activation_lock_path(self)?, false)?;
        match crate::platform::file_lock::try_lock_once_nonblocking(&file)
            .map_err(|e| e.to_string())?
        {
            true => Ok(Some(Lock(file))),
            false => Ok(None),
        }
    }
    pub(crate) fn acknowledge(&self, id: &str) -> Result<(), String> {
        self.path(id)?;
        if self.read(id)?.is_some()
            || self
                .read_path(id, &self.root.join("done").join(format!("{id}.json")))?
                .is_some()
        {
            Ok(())
        } else {
            Err("agenda job not found".into())
        }
    }
    pub(crate) fn recover_interrupted(&self, id: &str) -> Result<Job, String> {
        let _lock = self.lock(id)?;
        let mut job = self.read(id)?.ok_or("interrupted job disappeared")?;
        if job.status != "running" {
            return Ok(job);
        }
        job.result = Some(
            json!({"ok":false,"submission":"unknown","error":"executor interrupted; occurrence was not replayed"}),
        );
        if let Some(ms) = job.interval_ms {
            let interval = Duration::try_milliseconds(ms).ok_or("invalid interval")?;
            job.due_at = Utc::now()
                .checked_add_signed(interval)
                .ok_or("interval overflow")?
                .to_rfc3339();
            job.occurrence = job.occurrence.checked_add(1).ok_or("occurrence overflow")?;
            job.status = "active".into();
            self.write(&job)?;
        } else {
            job.status = "failed".into();
            self.archive(&job)?;
        }
        Ok(job)
    }
    fn archive(&self, job: &Job) -> Result<(), String> {
        self.write(job)?;
        let done = self.root.join("done");
        seat_store::private_dir(&done)?;
        fs::rename(self.path(&job.id)?, done.join(format!("{}.json", job.id)))
            .map_err(|e| e.to_string())?;
        seat_store::sync_directory(&done)?;
        seat_store::sync_directory(&self.root.join("jobs"))
    }
    pub(crate) fn entries(&self) -> Result<Vec<(String, Result<Job, String>)>, String> {
        let entries = match fs::read_dir(self.root.join("jobs")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut jobs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|s| s.strip_suffix(".json")) else {
                continue;
            };
            let result = self
                .read(id)
                .and_then(|job| job.ok_or_else(|| "job disappeared".into()));
            jobs.push((id.to_owned(), result));
        }
        jobs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(jobs)
    }
    pub fn list(&self) -> Result<Vec<Value>, String> {
        Ok(self
            .entries()?
            .into_iter()
            .map(|(id, result)| match result {
                Ok(job) => job.view(),
                Err(error) => json!({"id":id,"status":"unreadable","error":error}),
            })
            .collect())
    }
    pub fn add(&self, text: &str, raw_duration: &str, recurring: bool) -> Result<Value, String> {
        if text.trim().is_empty() || text.len() > 64 * 1024 {
            return Err("invalid reminder text".into());
        }
        let interval = parse_duration(raw_duration)?;
        let now = Utc::now();
        let due = now
            .checked_add_signed(interval)
            .ok_or("duration exceeds supported date range")?;
        let job = Job {
            v: 1,
            requires: CAPABILITIES.iter().map(|s| (*s).into()).collect(),
            id: format!("sched_{}", seat_store::random_instance()?),
            workspace: self.workspace.to_string_lossy().into_owned(),
            team: self.team.clone(),
            text: text.into(),
            trigger: format!("{} {raw_duration}", if recurring { "every" } else { "in" }),
            created_at: now.to_rfc3339(),
            due_at: due.to_rfc3339(),
            interval_ms: recurring.then_some(interval.num_milliseconds()),
            occurrence: 0,
            status: "active".into(),
            result: None,
            extra: BTreeMap::new(),
        };
        // The activation entity serializes the last-job exit/new-job publication race.
        let _activation = self.lock("activation")?;
        if !self.workspace.is_dir() {
            return Err("agenda workspace disappeared before publication".into());
        }
        self.write(&job)?;
        supervisor::mark_pending(self)?;
        let activation =
            supervisor::activate(self).and_then(|()| runtime::wake(self, Some(&job.id)));
        let mut receipt = job.view();
        receipt["stored"] = json!(true);
        match activation {
            Ok(()) => {
                receipt["ok"] = json!(true);
                receipt["activation"] = json!("supervised");
            }
            Err(error) => {
                receipt["ok"] = json!(false);
                receipt["activation"] = json!("inactive");
                receipt["error"] = json!(error);
            }
        }
        Ok(receipt)
    }
    pub fn cancel(&self, id: &str) -> Result<bool, String> {
        self.path(id)?;
        // Missing cancellation is read-only, including a never-used agenda directory.
        if self.read(id)?.is_none() {
            return Ok(false);
        }
        let _lock = self.lock(id)?;
        let Some(mut job) = self.read(id)? else {
            return Ok(false);
        };
        if job.status != "active" {
            return Ok(false);
        }
        job.status = "cancelled".into();
        self.archive(&job)?;
        let _ = runtime::notify(self);
        Ok(true)
    }
    pub(crate) fn fire(&self, id: &str) -> Result<Value, String> {
        let mut job = {
            let _lock = self.lock(id)?;
            let Some(mut job) = self.read(id)? else {
                return Ok(json!({"cancelled":true}));
            };
            if job.status != "active" || job.due()? > Utc::now() {
                return Ok(json!({"not_due":true}));
            }
            job.status = "running".into();
            self.write(&job)?;
            job
        };
        let claimed_occurrence = job.occurrence;
        // Running occurrences are never automatically replayed after a crash.
        let request_id = format!("agenda:{}:{}", job.id, job.occurrence);
        let presentation = crate::messaging::presentation::PresentationRequest::default();
        let result = super::send::execute(
            super::send::Request {
                workspace: &self.workspace,
                team: Some(&self.team),
                to: "leader",
                content: &job.text,
                task_id: None,
                message_id: Some(&request_id),
                mailbox: false,
                presentation: &presentation,
                envelope: None,
            },
            super::send::Authority::Operator {
                sender: "scheduler",
            },
        );
        let _lock = self.lock(id)?;
        // Preserve additions written by a newer compatible writer while execution was in flight.
        job = self.read(id)?.ok_or("claimed agenda job disappeared")?;
        if job.status != "running" || job.occurrence != claimed_occurrence {
            return Err("agenda claim changed during execution".into());
        }
        match result {
            Ok(receipt) => {
                job.result = Some(receipt);
                if let Some(ms) = job.interval_ms {
                    let interval = Duration::try_milliseconds(ms).ok_or("invalid interval")?;
                    job.due_at = Utc::now()
                        .checked_add_signed(interval)
                        .ok_or("interval overflow")?
                        .to_rfc3339();
                    job.occurrence = job.occurrence.checked_add(1).ok_or("occurrence overflow")?;
                    job.status = "active".into();
                    self.write(&job)?;
                } else {
                    job.status = "done".into();
                    self.archive(&job)?;
                }
            }
            Err(error) => {
                job.result = Some(json!({"ok":false,"error":error,"submission":"unknown"}));
                job.status = "failed".into();
                self.archive(&job)?;
            }
        }
        Ok(job.view())
    }
}

pub(crate) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = crate::platform::file_lock::unlock(&self.0);
    }
}

pub(crate) fn parse_duration(raw: &str) -> Result<Duration, String> {
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (amount, unit) = raw.split_at(split);
    let multiplier = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => 0,
    };
    amount.parse::<i64>().ok().and_then(|n| n.checked_mul(multiplier)).filter(|ms| *ms > 0)
        .and_then(Duration::try_milliseconds)
        .filter(|duration| Utc::now().checked_add_signed(*duration).is_some())
        .ok_or_else(|| "duration must be a positive integer followed by ms, s, m, h or d, within the supported date range".into())
}

#[cfg(test)]
mod tests;
