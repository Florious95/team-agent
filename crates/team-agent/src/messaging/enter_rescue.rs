//! Durable independent Enter-only watchdog. An uncertain effect is never replayed.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::delivery::{render_message, top_level_state_matches_owner_team};
use super::MessagingError;
use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::enter_rescue::{EnterRescueContext, EnterRescueOutcome};
use crate::transport::Transport;

#[cfg(test)]
#[path = "enter_rescue_tests.rs"]
mod tests;

const DELAY_SECONDS: i64 = 90;
const EXPIRE_SECONDS: i64 = 120;

#[derive(Debug, Serialize, Deserialize)]
struct RescueJob {
    schema: u8,
    context: EnterRescueContext,
    registered_at: String,
    expires_at: String,
}

fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, MessagingError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| MessagingError::Validation("enter_rescue: invalid timestamp".into()))
}

/// Called in the message-status transaction, even when no carrier/proof exists.
/// One original registration survives duplicates, unknown effects and restarts.
pub(crate) fn schedule_in_tx(
    tx: &Transaction<'_>,
    message_id: &str,
    now: DateTime<Utc>,
) -> Result<(), rusqlite::Error> {
    let row = tx.query_row(
        "select owner_team_id,coalesce(recipient,''),coalesce(sender,''),task_id,coalesce(content,'')
         from messages where message_id=?1 and status='submitted_unverified'",
        [message_id],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?,
            row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, String>(4)?)),
    ).optional()?;
    let Some((owner, recipient, sender, task, content)) = row else {
        return Ok(());
    };
    let job = RescueJob {
        schema: 1,
        context: EnterRescueContext {
            message_id: message_id.into(),
            owner_team_id: owner.clone().unwrap_or_default(),
            recipient: recipient.clone(),
            payload_sha256: digest(&render_message(
                &sender,
                task.as_deref(),
                &content,
                message_id,
            )),
            target: None,
            proof: None,
        },
        registered_at: now.to_rfc3339(),
        expires_at: (now + Duration::seconds(EXPIRE_SECONDS)).to_rfc3339(),
    };
    let payload = serde_json::to_string(&job)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    tx.execute(
        "insert into scheduled_events(kind,target,payload_json,due_at,status,owner_team_id,created_at)
         select 'enter_rescue',?1,?2,?3,'pending',?4,?5 where not exists(
           select 1 from scheduled_events where kind='enter_rescue' and case when json_valid(payload_json)
           then coalesce(json_extract(payload_json,'$.context.message_id'),json_extract(payload_json,'$.evidence.request.message_id')) end=?6)",
        params![recipient, payload, (now + Duration::seconds(DELAY_SECONDS)).to_rfc3339(), owner,
            now.to_rfc3339(), message_id],
    )?;
    Ok(())
}

/// Supplement the original job at the original delivery boundary, never at fire.
/// This cannot change its scope, message digest, initial clock, or a bound carrier.
pub(crate) fn bind_pending_context(
    store: &MessageStore,
    context: &EnterRescueContext,
) -> Result<bool, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

fn canonical_valid(
    workspace: &Path,
    store: &MessageStore,
    job: &RescueJob,
) -> Result<bool, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

pub(super) fn fire(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    log: &EventLog,
    id: i64,
    payload: &str,
) -> Result<bool, MessagingError> {
    fire_with(workspace, store, log, id, payload, &|context, authorize| {
        super::delivery::with_rescue_submit_observer(workspace, store, context, log, |observer| {
            transport
                .rescue_enter_guarded(context, authorize, Some(observer))
                .map_err(Into::into)
        })
    })
}

type Executor<'a> = dyn Fn(&EnterRescueContext, &dyn Fn() -> bool) -> Result<EnterRescueOutcome, MessagingError>
    + 'a;

fn fire_with(
    workspace: &Path,
    store: &MessageStore,
    log: &EventLog,
    id: i64,
    payload: &str,
    execute: &Executor<'_>,
) -> Result<bool, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}
