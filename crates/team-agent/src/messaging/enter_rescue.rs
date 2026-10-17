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
    let conn = crate::db::schema::open_db(store.db_path())?;
    let row = conn.query_row(
        "select id,payload_json from scheduled_events where kind='enter_rescue' and status='pending'
         and case when json_valid(payload_json) then json_extract(payload_json,'$.context.message_id') end=?1",
        [&context.message_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    ).optional()?;
    let Some((id, previous)) = row else {
        return Ok(false);
    };
    let mut job: RescueJob = serde_json::from_str(&previous)?;
    if job.schema != 1
        || job.context.message_id != context.message_id
        || job.context.owner_team_id != context.owner_team_id
        || job.context.recipient != context.recipient
        || job.context.payload_sha256 != context.payload_sha256
        || job.context.target.as_ref().is_some_and(|target| {
            !context
                .target
                .as_ref()
                .is_some_and(|next| target.same_selection(next))
                || (target.bound() && job.context != *context)
        })
    {
        return Ok(false);
    }
    job.context = context.clone();
    Ok(conn.execute(
        "update scheduled_events set payload_json=?2 where id=?1 and status='pending' and payload_json=?3
         and exists(select 1 from messages where message_id=?4 and status='submitted_unverified'
             and owner_team_id=?5 and recipient=?6)",
        params![id, serde_json::to_string(&job)?, previous, context.message_id,
            context.owner_team_id, context.recipient],
    )? == 1)
}

fn canonical_valid(
    workspace: &Path,
    store: &MessageStore,
    job: &RescueJob,
) -> Result<bool, MessagingError> {
    let context = &job.context;
    let registered = timestamp(&job.registered_at)?;
    let expires = timestamp(&job.expires_at)?;
    let now = Utc::now();
    if job.schema != 1
        || !context.valid_message()
        || expires != registered + Duration::seconds(EXPIRE_SECONDS)
        || now < registered + Duration::seconds(DELAY_SECONDS)
        || now > expires
    {
        return Ok(false);
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let original = conn
        .query_row(
            "select sender,task_id,content,created_at from messages where message_id=?1
         and owner_team_id=?2 and recipient=?3 and status='submitted_unverified'",
            params![context.message_id, context.owner_team_id, context.recipient],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((sender, task, content, created_at)) = original else {
        return Ok(false);
    };
    if digest(&render_message(
        &sender,
        task.as_deref(),
        &content,
        &context.message_id,
    )) != context.payload_sha256
    {
        return Ok(false);
    }
    let newer: bool = conn.query_row(
        "select exists(select 1 from messages where owner_team_id=?1 and recipient=?2
         and message_id<>?3 and created_at>=?4 and delivery_attempts>0)",
        params![
            context.owner_team_id,
            context.recipient,
            context.message_id,
            created_at
        ],
        |row| row.get(0),
    )?;
    if newer {
        return Ok(false);
    }
    let raw = crate::state::persist::load_runtime_state(workspace)?;
    if !matches!(crate::state::projection::resolve_owner_team_id(&raw, &context.owner_team_id),
        crate::state::projection::OwnerTeamResolution::Canonical(team) if team == context.owner_team_id)
    {
        return Ok(false);
    }
    let state = if raw
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.contains_key(&context.owner_team_id))
    {
        crate::state::projection::project_top_level_view(&raw, &context.owner_team_id)
    } else if top_level_state_matches_owner_team(&raw, &context.owner_team_id) {
        raw
    } else {
        return Ok(false);
    };
    let Some(agent) = state
        .get("agents")
        .and_then(|agents| agents.get(&context.recipient))
    else {
        return Ok(false);
    };
    if agent.get("status").and_then(serde_json::Value::as_str) != Some("running")
        || agent
            .get("explicit_stop")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return Ok(false);
    }
    // An unbound job is still an observable scheduled job, but never key authority.
    let Some(target) = context.target.as_ref() else {
        return Ok(true);
    };
    Ok(
        agent.get("pane_id").and_then(serde_json::Value::as_str) == Some(target.pane_id.as_str())
            && agent.get("pane_pid").and_then(serde_json::Value::as_u64)
                == Some(u64::from(target.pane_pid))
            && state
                .get("session_name")
                .and_then(serde_json::Value::as_str)
                == Some(target.session.as_str())
            && agent
                .get("window")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&context.recipient)
                == target.window
            && agent.get("provider").cloned().and_then(|value| {
                serde_json::from_value::<crate::model::enums::Provider>(value).ok()
            }) == Some(target.provider),
    )
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
    let conn = crate::db::schema::open_db(store.db_path())?;
    if conn.execute(
        "update scheduled_events set status='claimed',fired_at=?2 where id=?1 and kind='enter_rescue'
         and status='pending' and due_at<=?2 and payload_json=?3",
        params![id, Utc::now().to_rfc3339(), payload],
    )? != 1 { return Ok(false); }
    log.write("enter_rescue.claimed", serde_json::json!({"event_id":id}))?;
    let result = (|| -> Result<(String, serde_json::Value, Option<String>), MessagingError> {
        let job: RescueJob = serde_json::from_str(payload)?;
        let context = &job.context;
        let scope_matches: bool = conn.query_row(
            "select coalesce(owner_team_id,'')=?2 and target=?3 and created_at=?4 from scheduled_events where id=?1",
            params![id, context.owner_team_id, context.recipient, job.registered_at], |row| row.get(0),
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
        if !context.target.as_ref().is_some_and(|target| target.bound()) || !context.valid_proof() {
            return Ok((
                "refused".into(),
                serde_json::json!({"reason":"proof_missing","effect":"none"}),
                None,
            ));
        }
        let authorize = || {
            let still_claimed: bool = conn.query_row(
                "select exists(select 1 from scheduled_events where id=?1 and kind='enter_rescue'
                 and status='claimed' and payload_json=?2 and owner_team_id=?3 and target=?4)",
                params![id, payload, context.owner_team_id, context.recipient], |row| row.get(0),
            ).unwrap_or(false);
            still_claimed && canonical_valid(workspace, store, &job).unwrap_or(false)
        };
        match execute(context, &authorize)? {
            EnterRescueOutcome::NoEffect { reason, reobserve } => {
                let next = Utc::now() + Duration::seconds(5);
                let due =
                    (reobserve && next <= timestamp(&job.expires_at)?).then(|| next.to_rfc3339());
                Ok((
                    if due.is_some() { "pending" } else { "refused" }.into(),
                    serde_json::json!({"reason":reason,"effect":"none"}),
                    due,
                ))
            }
            EnterRescueOutcome::Consumed | EnterRescueOutcome::Entered { accepted: true } => {
                let changed = conn.execute(
                    "update messages set status='delivered',delivered_at=?2,updated_at=?2,error=null
                     where message_id=?1 and status='submitted_unverified' and owner_team_id=?3 and recipient=?4",
                    params![context.message_id, Utc::now().to_rfc3339(), context.owner_team_id, context.recipient],
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
    let (status, result, due) = result.unwrap_or_else(|error| {
        (
            "unknown".into(),
            serde_json::json!({"error":error.to_string(),"retry":false}),
            None,
        )
    });
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
