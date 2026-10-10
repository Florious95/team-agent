//! Trusted local operator ingress into the same durable outbox as the three MCP
//! tools. Transport/root ownership is checked by the public runtime before using
//! this port; a provider cannot obtain an operator identity through tool arguments.
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::store::{current, load_seat, next_id, unleased, ContractStore, SeatStatus};
use super::Error;
use crate::contract::{delivery::LogicalEnvelope, types::*};
use crate::runtime::delivery::PreparedEnvelope;

pub(crate) const MESSAGE_PRESENTATION: &str = "{\"sink\":\"leader\",\"class\":\"message\"}";

pub(crate) struct MessageWrite<'a> {
    pub id: Option<&'a MessageId>,
    pub source: Option<&'a InstanceIdentity>,
    pub task: Option<&'a str>,
    pub sender: &'a str,
    pub recipient: &'a str,
    pub content: &'a str,
    pub presentation: &'a str,
    pub mailbox: bool,
}

/// One transaction-owned message primitive for operator send, MCP send and
/// result notification. Missing leader remains an unbound notification; other
/// missing recipients are errors. No delivery or readiness is fabricated here.
pub(crate) fn write_message(
    tx: &Transaction<'_>,
    scope: &ScopeId,
    message: MessageWrite<'_>,
) -> Result<String, Error> {
    let external = super::forward::peer(tx, message.recipient)?;
    let target = if external.is_none() {
        load_seat(tx, &SeatId::new(message.recipient)?)?
    } else {
        None
    };
    if target.is_none() && external.is_none() && message.recipient != "leader" {
        return Err(Error::Invalid("unknown recipient"));
    }
    if target.as_ref().is_some_and(|s| &s.identity.scope != scope) {
        return Err(Error::Fence);
    }
    let id = match message.id {
        Some(id) => id.as_str().to_string(),
        None => next_id(tx, "msg")?,
    };
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id=?1)",
        [&id],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error::Conflict);
    }
    tx.execute("INSERT INTO messages(message_id,owner_team_id,task_id,sender,recipient,status,content,presentation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![id,scope.as_str(),message.task,message.sender,message.recipient,
            if external.is_some() {"forward_pending"} else if message.mailbox {"stored_only"} else {"accepted"},message.content,message.presentation])?;
    if let Some(external) = external {
        let source = message
            .source
            .ok_or(Error::Invalid("framework forwarding source"))?;
        if source.scope != *scope || source.seat.as_str() != message.sender {
            return Err(Error::Fence);
        }
        super::forward::insert(
            tx,
            &super::forward::ForwardIntent {
                id: id.clone(),
                source: source.clone(),
                route: external.route,
                payload: super::forward::ForwardPayload::Message {
                    message: MessageId::new(&id)?,
                    task: message.task.map(str::to_owned),
                    sender: message.sender.into(),
                    recipient: message.recipient.into(),
                    content: message.content.into(),
                    mailbox: message.mailbox,
                },
                state: super::forward::ForwardState::Pending,
                receipt: None,
            },
        )?;
    } else if !message.mailbox {
        let identity = target
            .map(|s| serde_json::to_string(&s.identity))
            .transpose()?
            .unwrap_or_else(|| "null".into());
        tx.execute(
            "INSERT INTO contract_outbox VALUES(?1,?2,'queued',NULL,'\"NoEffect\"')",
            params![id, identity],
        )?;
    }
    Ok(id)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorSend {
    pub recipient: SeatId,
    pub content: String,
    pub task: Option<String>,
    pub message: Option<MessageId>,
    pub mailbox: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    pub message: MessageId,
    pub task: String,
    pub target: InstanceIdentity,
    pub mailbox: bool,
}

impl ContractStore {
    /// Fenced local send. The identity stored with the outbox is resolved in the
    /// same transaction as insertion, never retargeted to a later generation.
    pub fn send_from_operator(&mut self, request: &OperatorSend) -> Result<SendReceipt, Error> {
        self.send_from_framework(request, &SeatId::new("leader")?)
    }

    /// A shared framework dispatcher supplies the captured sender. This is not
    /// exposed as a native tool argument and never routes through a legacy
    /// fallback. Destination generation and native queue are still atomic.
    pub fn send_from_framework(
        &mut self,
        request: &OperatorSend,
        sender: &SeatId,
    ) -> Result<SendReceipt, Error> {
        if !nonblank(&request.content)
            || request.content.len() > super::mcp::MAX_FRAME_BYTES
            || request
                .task
                .as_deref()
                .is_some_and(|s| !nonblank(s) || s.chars().any(char::is_control))
        {
            return Err(Error::Invalid("operator message"));
        }
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target =
            load_seat(&tx, &request.recipient)?.ok_or(Error::Invalid("unknown recipient"))?;
        if target.identity.scope != scope
            || matches!(target.status, SeatStatus::Stopped | SeatStatus::Unknown)
        {
            return Err(Error::Fence);
        }
        unleased(&tx, &request.recipient)?;
        let id = match &request.message {
            Some(id) => id.clone(),
            None => MessageId::new(next_id(&tx, "msg")?)?,
        };
        let task = request.task.clone().unwrap_or_else(|| id.as_str().into());
        let logical = LogicalEnvelope {
            message: id.clone(),
            sender: sender.clone(),
            task: Some(task.clone()),
            content: request.content.clone(),
        };
        // Reuse physical payload admission before durable acceptance. This does
        // not paste, reserve bootstrap or imply any native effect.
        PreparedEnvelope::from_logical(&logical)?;
        write_message(
            &tx,
            &scope,
            MessageWrite {
                id: Some(&id),
                source: None,
                task: Some(&task),
                sender: sender.as_str(),
                recipient: request.recipient.as_str(),
                content: &request.content,
                presentation: MESSAGE_PRESENTATION,
                mailbox: request.mailbox,
            },
        )?;
        tx.commit()?;
        Ok(SendReceipt {
            message: id,
            task,
            target: target.identity,
            mailbox: request.mailbox,
        })
    }

    /// A server may attribute a business tool call only to a submitted message
    /// for this exact generation. Queued/mailbox/uncertain records are not turns.
    /// The transport must additionally verify its native parent/connection.
    pub fn submitted_task(&mut self, identity: &InstanceIdentity) -> Result<Option<String>, Error> {
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let seat = current(&tx, &scope, identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        unleased(&tx, &identity.seat)?;
        let encoded = serde_json::to_string(identity)?;
        let task: Option<String> = tx
            .query_row(
                "SELECT m.task_id FROM messages m JOIN contract_outbox o USING(message_id)
             WHERE m.owner_team_id=?1 AND m.recipient=?2 AND o.target=?3
             AND o.state='submitted' AND m.task_id IS NOT NULL ORDER BY m.rowid DESC LIMIT 1",
                params![scope.as_str(), identity.seat.as_str(), encoded],
                |r| r.get(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(task)
    }

    /// Facts from one captured transport connection only. The caller still has
    /// to observe that connection's process/lifetime; these rows alone cannot
    /// refresh a stopped server or establish native client consumption.
    pub fn connection_facts(
        &mut self,
        identity: &InstanceIdentity,
        connection: &InstanceId,
    ) -> Result<Vec<String>, Error> {
        let scope = self.scope().clone();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let seat = current(&tx, &scope, identity)?;
        if matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
            return Err(Error::Fence);
        }
        let caller = serde_json::to_string(identity)?;
        let prefix = format!("{}:", connection.as_str());
        let facts = {
            let mut stmt = tx.prepare("SELECT DISTINCT fact FROM contract_facts WHERE caller=?1 AND substr(call_key,1,length(?2))=?2 ORDER BY fact")?;
            let rows = stmt.query_map(params![caller, prefix], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(facts)
    }

    /// A public inbox projection. Reading it neither delivers nor acknowledges
    /// queued work, and cannot manufacture a native response or presentation.
    pub fn inbox(&self, recipient: &SeatId, limit: usize) -> Result<Vec<Value>, Error> {
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("inbox limit"));
        }
        let mut stmt = self.connection.prepare(
            "SELECT message_id,task_id,sender,status,content FROM messages
             WHERE owner_team_id=?1 AND recipient=?2 ORDER BY rowid DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![self.scope().as_str(), recipient.as_str(), limit as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (message,task,sender,status,content) = row?;
            Ok(json!({"message_id":message,"task_id":task,"sender":sender,"status":status,"content":content}))
        }).collect()
    }
}
