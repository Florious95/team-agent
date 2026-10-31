
PRAGMA application_id=1413563187;
PRAGMA user_version=4;
CREATE TABLE contract_meta(scope TEXT NOT NULL, endpoint TEXT NOT NULL, root TEXT NOT NULL,
 incarnation TEXT NOT NULL, result_route TEXT);
CREATE TABLE contract_framework_peers(recipient TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE contract_forwards(id TEXT PRIMARY KEY, state TEXT NOT NULL, record TEXT NOT NULL);
CREATE TABLE contract_connections(connection TEXT PRIMARY KEY, identity TEXT NOT NULL, record TEXT NOT NULL);
CREATE TABLE contract_seats(seat TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE contract_generations(seat TEXT PRIMARY KEY, generation INTEGER NOT NULL);
CREATE TABLE contract_instances(instance TEXT PRIMARY KEY, seat TEXT NOT NULL, generation INTEGER NOT NULL);
CREATE TABLE contract_operations(id TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE contract_leases(seat TEXT PRIMARY KEY, operation TEXT NOT NULL);
CREATE TABLE contract_bootstrap(owner TEXT PRIMARY KEY, attempt TEXT NOT NULL UNIQUE,
 confirmed INTEGER NOT NULL CHECK(confirmed IN (0,1)));
CREATE TABLE contract_calls(caller TEXT NOT NULL, call_key TEXT NOT NULL, request TEXT NOT NULL, response TEXT NOT NULL,
 PRIMARY KEY(caller,call_key));
CREATE TABLE contract_facts(caller TEXT NOT NULL, call_key TEXT NOT NULL, fact TEXT NOT NULL,
 PRIMARY KEY(caller,call_key,fact));
CREATE TABLE contract_sequence(id INTEGER PRIMARY KEY CHECK(id=1), value INTEGER NOT NULL);
INSERT INTO contract_sequence VALUES(1,0);
CREATE TABLE messages(message_id TEXT PRIMARY KEY, owner_team_id TEXT NOT NULL, task_id TEXT,
 sender TEXT NOT NULL, recipient TEXT NOT NULL, status TEXT NOT NULL, content TEXT NOT NULL,
 presentation TEXT NOT NULL, delivery_attempts INTEGER NOT NULL DEFAULT 0);
CREATE TABLE results(result_id TEXT PRIMARY KEY, owner_team_id TEXT NOT NULL, task_id TEXT NOT NULL,
 agent_id TEXT NOT NULL, envelope TEXT NOT NULL, status TEXT NOT NULL,
 UNIQUE(owner_team_id,task_id,agent_id));
CREATE TABLE contract_outbox(message_id TEXT PRIMARY KEY REFERENCES messages(message_id), target TEXT NOT NULL,
 state TEXT NOT NULL, attempt TEXT, effect TEXT NOT NULL);
