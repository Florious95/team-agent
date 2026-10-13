//! Durable Enter-only recovery. A claimed/unknown job is never replayed as a paste.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::delivery::{render_message, top_level_state_matches_owner_team};
use super::MessagingError;
use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::native_input::{EnterRescueOutcome, NativeInputEvidence, NativeInputPhase};
use crate::transport::Transport;

#[cfg(test)]
#[path = "enter_rescue_tests.rs"]
mod tests;

const DELAY_SECONDS: i64 = 90;
const EXPIRE_SECONDS: i64 = 120;

#[derive(Debug, Serialize, Deserialize)]
struct RescueJob {
    evidence: NativeInputEvidence,
    expires_at: String,
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn expiration(job: &RescueJob) -> Result<DateTime<Utc>, MessagingError> {
    DateTime::parse_from_rfc3339(&job.expires_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| MessagingError::Validation("enter_rescue: invalid expiry".into()))
}

/// The claim consumed no physical attempt. Never use this after a paste intent.
pub(super) fn release_no_effect(
    store: &MessageStore,
    message_id: &str,
    attempt: u32,
) -> Result<(), MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "update messages set error='native_input_not_ready', delivery_attempts=delivery_attempts-1, updated_at=?3
         where message_id=?1 and status='target_resolved' and error is null and delivery_attempts=?2",
        params![message_id, attempt, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Original uncertainty and its one job are committed together under the same CAS.
pub(super) fn park(
    store: &MessageStore,
    evidence: &NativeInputEvidence,
    attempt: u32,
    reason: &str,
    log: &EventLog,
) -> Result<(), MessagingError> {
    park_at(store, evidence, attempt, reason, log, Utc::now())
}

fn park_at(
    store: &MessageStore,
    evidence: &NativeInputEvidence,
    attempt: u32,
    reason: &str,
    log: &EventLog,
    now: DateTime<Utc>,
) -> Result<(), MessagingError> {
    if !evidence.request.supported() || evidence.phase < NativeInputPhase::MayHavePasted {
        return Err(MessagingError::Validation(
            "enter_rescue: owned proof missing".into(),
        ));
    }
    let mut conn = crate::db::schema::open_db(store.db_path())?;
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "update messages set status='submitted_unverified', error=?4, updated_at=?5
         where message_id=?1 and owner_team_id=?2 and recipient=?3
           and status='target_resolved' and error is null and delivery_attempts=?6",
        params![
            evidence.request.message_id,
            evidence.request.owner_team_id,
            evidence.request.recipient,
            reason,
            now.to_rfc3339(),
            attempt
        ],
    )?;
    if changed != 1 {
        return Err(MessagingError::Validation(
            "enter_rescue: claim changed".into(),
        ));
    }
    let job = RescueJob {
        evidence: evidence.clone(),
        expires_at: (now + Duration::seconds(EXPIRE_SECONDS)).to_rfc3339(),
    };
    tx.execute(
        "insert into scheduled_events(kind,target,payload_json,due_at,status,owner_team_id,created_at) values('enter_rescue',?1,?2,?3,'pending',?4,?5)",
        params![evidence.request.recipient, serde_json::to_string(&job)?,
            (now + Duration::seconds(DELAY_SECONDS)).to_rfc3339(), evidence.request.owner_team_id, now.to_rfc3339()],
    )?;
    tx.commit()?;
    log.write("enter_rescue.scheduled", serde_json::json!({
        "message_id": evidence.request.message_id, "recipient": evidence.request.recipient,
        "phase": evidence.phase, "delay_seconds": DELAY_SECONDS, "expire_seconds": EXPIRE_SECONDS,
    }))?;
    Ok(())
}

fn canonical_valid(
    workspace: &Path,
    store: &MessageStore,
    job: &RescueJob,
) -> Result<bool, MessagingError> {
    let evidence = &job.evidence;
    let request = &evidence.request;
    if Utc::now() > expiration(job)? || !request.supported() {
        return Ok(false);
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let original = conn.query_row(
        "select sender, task_id, content, created_at from messages
         where message_id=?1 and owner_team_id=?2 and recipient=?3 and status='submitted_unverified'",
        params![request.message_id, request.owner_team_id, request.recipient],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
    ).optional()?;
    let Some((sender, task, content, created_at)) = original else {
        return Ok(false);
    };
    if digest(&render_message(
        &sender,
        task.as_deref(),
        &content,
        &request.message_id,
    )) != evidence.payload_sha256
    {
        return Ok(false);
    }
    let newer: bool = conn.query_row(
        "select exists(select 1 from messages where owner_team_id=?1 and recipient=?2
         and message_id<>?3 and created_at>=?4 and delivery_attempts>0)",
        params![
            request.owner_team_id,
            request.recipient,
            request.message_id,
            created_at
        ],
        |row| row.get(0),
    )?;
    if newer {
        return Ok(false);
    }
    let raw = crate::state::persist::load_runtime_state(workspace)?;
    if !matches!(crate::state::projection::resolve_owner_team_id(&raw, &request.owner_team_id),
        crate::state::projection::OwnerTeamResolution::Canonical(team) if team == request.owner_team_id)
    {
        return Ok(false);
    }
    let state = if raw
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.contains_key(&request.owner_team_id))
    {
        crate::state::projection::project_top_level_view(&raw, &request.owner_team_id)
    } else if top_level_state_matches_owner_team(&raw, &request.owner_team_id) {
        raw
    } else {
        return Ok(false);
    };
    let Some(agent) = state
        .get("agents")
        .and_then(|agents| agents.get(&request.recipient))
    else {
        return Ok(false);
    };
    Ok(
        agent.get("status").and_then(serde_json::Value::as_str) == Some("running")
            && agent
                .get("explicit_stop")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
            && agent.get("pane_id").and_then(serde_json::Value::as_str)
                == Some(request.pane_id.as_str())
            && agent.get("pane_pid").and_then(serde_json::Value::as_u64)
                == Some(u64::from(request.pane_pid))
            && request.pane_pid == evidence.binding.pane_pid
            && state
                .get("session_name")
                .and_then(serde_json::Value::as_str)
                == Some(request.session.as_str())
            && agent
                .get("window")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&request.recipient)
                == request.window
            && agent.get("provider").cloned().and_then(|value| {
                serde_json::from_value::<crate::model::enums::Provider>(value).ok()
            }) == Some(request.provider),
    )
}

/// Called only for due jobs. CAS precedes every possible effect; stale `claimed`
/// rows intentionally remain unknown after a crash, rather than becoming runnable.
pub(super) fn fire(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    log: &EventLog,
    id: i64,
    payload: &str,
) -> Result<bool, MessagingError> {
    fire_with(
        workspace,
        store,
        log,
        id,
        payload,
        &|evidence, authorize| {
            super::delivery::with_rescue_submit_observer(
                workspace,
                store,
                evidence,
                log,
                |observer| {
                    transport
                        .rescue_enter(evidence, authorize, Some(observer))
                        .map_err(Into::into)
                },
            )
        },
    )
}

fn fire_with(
    workspace: &Path,
    store: &MessageStore,
    log: &EventLog,
    id: i64,
    payload: &str,
    execute: &dyn Fn(
        &NativeInputEvidence,
        &dyn Fn() -> bool,
    ) -> Result<EnterRescueOutcome, MessagingError>,
) -> Result<bool, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    if conn.execute(
        "update scheduled_events set status='claimed', fired_at=?2
         where id=?1 and kind='enter_rescue' and status='pending' and due_at<=?2 and payload_json=?3",
        params![id, Utc::now().to_rfc3339(), payload],
    )? != 1 { return Ok(false); }
    log.write("enter_rescue.claimed", serde_json::json!({"event_id": id}))?;
    let result = (|| -> Result<(String, serde_json::Value, Option<String>), MessagingError> {
        let job: RescueJob = serde_json::from_str(payload)?;
        let scope_matches: bool = conn.query_row(
            "select owner_team_id=?2 and target=?3 from scheduled_events where id=?1",
            params![
                id,
                job.evidence.request.owner_team_id,
                job.evidence.request.recipient
            ],
            |row| row.get(0),
        )?;
        if !scope_matches {
            return Ok((
                "refused".into(),
                serde_json::json!({"reason":"job_scope_changed"}),
                None,
            ));
        }
        if !canonical_valid(workspace, store, &job)? {
            return Ok((
                "refused".into(),
                serde_json::json!({"reason":"canonical_changed_or_expired"}),
                None,
            ));
        }
        let authorize = || canonical_valid(workspace, store, &job).unwrap_or(false);
        let outcome = execute(&job.evidence, &authorize)?;
        match outcome {
            EnterRescueOutcome::NoEffect { reason, reobserve } => {
                let next = Utc::now() + Duration::seconds(5);
                let due = (reobserve && next <= expiration(&job)?).then(|| next.to_rfc3339());
                Ok((
                    if due.is_some() { "pending" } else { "refused" }.into(),
                    serde_json::json!({"reason":reason,"effect":"none"}),
                    due,
                ))
            }
            EnterRescueOutcome::Consumed | EnterRescueOutcome::Entered { accepted: true } => {
                // Row-level CAS cannot downgrade an acknowledgement/other terminal owner.
                let changed = conn.execute(
                    "update messages set status='delivered',delivered_at=?2,updated_at=?2,error=null
                     where message_id=?1 and status='submitted_unverified' and owner_team_id=?3 and recipient=?4",
                    params![job.evidence.request.message_id, Utc::now().to_rfc3339(),
                        job.evidence.request.owner_team_id, job.evidence.request.recipient],
                )?;
                Ok((
                    "done".into(),
                    serde_json::json!({"accepted":true,"message_updated":changed==1}),
                    None,
                ))
            }
            EnterRescueOutcome::Entered { accepted: false } => Ok((
                "unknown".into(),
                serde_json::json!({"accepted":false,"effect":"may_have_submitted"}),
                None,
            )),
        }
    })();
    let (status, result, due) = match result {
        Ok(result) => result,
        Err(error) => (
            "unknown".into(),
            serde_json::json!({"error":error.to_string(),"retry":false}),
            None,
        ),
    };
    conn.execute(
        "update scheduled_events set status=?2,result_json=?3,due_at=coalesce(?4,due_at)
         where id=?1 and kind='enter_rescue' and status='claimed'",
        params![id, status, result.to_string(), due],
    )?;
    log.write(
        "enter_rescue.result",
        serde_json::json!({"event_id":id,"status":status,"result":result}),
    )?;
    Ok(true)
}
