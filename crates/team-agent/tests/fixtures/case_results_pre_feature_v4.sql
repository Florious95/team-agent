pragma journal_mode = wal;

create table messages (
  message_id text primary key,
  owner_team_id text,
  task_id text,
  sender text,
  recipient text,
  reply_to text,
  requires_ack integer,
  status text,
  content text,
  presentation text not null default '{"sink":"leader","class":"message"}',
  artifact_refs text,
  created_at text,
  updated_at text,
  delivered_at text,
  acknowledged_at text,
  error text,
  delivery_attempts integer not null default 0
);

create table results (
  result_id text primary key,
  owner_team_id text,
  task_id text not null,
  agent_id text not null,
  envelope text not null,
  status text not null,
  created_at text not null
);

create table scheduled_events (
  id integer primary key,
  owner_team_id text,
  due_at text not null,
  target text not null,
  kind text not null,
  payload_json text not null,
  status text not null,
  created_at text not null,
  fired_at text,
  result_json text
);

create table delivery_tokens (
  message_id text primary key,
  unique_token text not null,
  injected_at text not null,
  visible_at text,
  consumed_at text,
  failed_at text,
  failure_reason text
);

create table agent_health (
  owner_team_id text,
  agent_id text not null,
  status text not null,
  last_output_at text,
  context_usage_pct integer,
  current_task_id text,
  updated_at text not null,
  unique(owner_team_id, agent_id)
);

create table peer_allowlist (
  a text not null,
  b text not null,
  created_at text not null,
  primary key (a, b)
);

create table result_watchers (
  watcher_id text primary key,
  owner_team_id text,
  task_id text,
  agent_id text,
  message_id text,
  leader_id text not null,
  status text not null,
  created_at text not null,
  completed_at text,
  result_id text,
  notified_message_id text,
  error text
);

create table leader_notification_log (
  result_id text not null,
  owner_team_id text not null default '',
  owner_epoch integer not null default 0,
  leader_session_uuid text,
  notified_message_id text not null,
  notified_at text not null,
  leader_pane_id_at_notify text,
  envelope_content_hash text,
  primary key (result_id, owner_team_id, owner_epoch)
);

create index idx_leader_notification_log_uuid
  on leader_notification_log(leader_session_uuid, notified_at);
create index idx_leader_notification_log_team_epoch
  on leader_notification_log(owner_team_id, owner_epoch, notified_at);
create index idx_messages_owner_team_id on messages(owner_team_id);
create index idx_scheduled_events_owner_team_id on scheduled_events(owner_team_id);
create index idx_agent_health_owner_team_id on agent_health(owner_team_id);
create index idx_result_watchers_owner_team_id on result_watchers(owner_team_id);

insert into results(
  result_id,
  owner_team_id,
  task_id,
  agent_id,
  envelope,
  status,
  created_at
) values (
  'res-pre-feature-v4',
  'teamA',
  'case-pre-feature',
  'worker_a',
  '{"schema_version":"result_envelope_v1","result_id":"res-pre-feature-v4","task_id":"case-pre-feature","agent_id":"worker_a","status":"archived/custom","summary":"PRE_FEATURE_V4_CANARY","changes":[],"tests":[],"risks":[],"artifacts":[{"path":"artifact://pre-feature/report.md","meta":{"depth":{"kept":true}}}],"next_actions":[],"presentation":{"sink":"casefile","class":"stage_result","case_id":"case-pre-feature"},"created_at":"2026-07-29T00:00:00.000Z"}',
  'archived/custom',
  '2026-07-29T00:00:00.000Z'
);

pragma user_version = 4;
