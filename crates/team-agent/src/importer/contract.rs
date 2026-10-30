//! Explicit M3 contract migration. SQLite is read-only and never runtime authority.
//! Run during a maintenance window; do not issue old-release lifecycle commands
//! concurrently. Provider processes are not spawned, replayed, or killed here.
use rusqlite::{Connection, OpenFlags};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Duration};
use team_agent_contract::{
    contract::types::*,
    host::{digest_hex, process::fingerprint_file},
    orchestration::store::{LegacySnapshot, ProjectionStore},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn records<T: DeserializeOwned>(db: &Connection, table: &str) -> Result<Vec<T>> {
    let mut query = db.prepare(&format!("SELECT record FROM {table} ORDER BY rowid"))?;
    let rows = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|row| serde_json::from_str(&row).map_err(Into::into))
        .collect()
}

pub fn migrate(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Value> {
    if root.join("contract.json").exists() {
        let store = ProjectionStore::open_readonly(root, scope, endpoint)?;
        return Ok(
            json!({"ok":true,"status":"already_migrated","seats":store.seats()?.len(),"root":root,"replayed":0}),
        );
    }
    // Resume a killed importer from its immutable logical snapshot, not a new
    // live database snapshot. The core serializes publication and checks scope.
    let archive = root.join("legacy-import.json");
    let snapshot: LegacySnapshot = if archive.exists() {
        let metadata = std::fs::symlink_metadata(&archive)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 16 * 1024 * 1024
        {
            return Err("invalid legacy import snapshot".into());
        }
        serde_json::from_slice(&std::fs::read(&archive)?)?
    } else {
        read_snapshot(root, &scope, endpoint)?
    };
    ProjectionStore::import_legacy(root, scope.clone(), endpoint, snapshot)?;
    let store = ProjectionStore::open_readonly(root, scope, endpoint)?;
    Ok(
        json!({"ok":true,"status":"migrated","root":root,"snapshot":archive,
        "seats":store.seats()?.len(),"unfinished_operations":store.unfinished()?.len(),
        "legacy_database":"retained_read_only","retired_outbox":"not_imported","replayed":0}),
    )
}

fn read_snapshot(
    root: &Path,
    expected_scope: &ScopeId,
    expected_endpoint: &str,
) -> Result<LegacySnapshot> {
    let path = root.join("contract.db");
    let source_digest = digest_hex(fingerprint_file(
        &path,
        1024 * 1024 * 1024,
        Duration::from_secs(10),
    )?);
    let mut db = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_secs(2))?;
    // One read transaction gives a consistent WAL-aware logical snapshot.
    let tx = db.transaction()?;
    let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != 0x54414333 || version != 4 {
        return Err("unsupported legacy contract store schema".into());
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM contract_meta", [], |r| r.get(0))?;
    if count != 1 {
        return Err("invalid legacy metadata cardinality".into());
    }
    let (scope, endpoint, original_root, incarnation, result_route): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = tx.query_row(
        "SELECT scope,endpoint,root,incarnation,result_route FROM contract_meta",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let scope = ScopeId::new(scope)?;
    if scope != *expected_scope
        || endpoint != expected_endpoint
        || Path::new(&original_root) != root
    {
        return Err("legacy contract scope, endpoint or root mismatch".into());
    }
    let mut query = tx.prepare("SELECT seat,generation FROM contract_generations")?;
    let generations = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    let mut query = tx.prepare("SELECT instance,seat,generation FROM contract_instances")?;
    let instances = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(instance, seat, generation)| {
            Ok(InstanceIdentity {
                scope: scope.clone(),
                instance: InstanceId::new(instance)?,
                seat: SeatId::new(seat)?,
                generation: Generation(generation),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut query = tx.prepare("SELECT seat,operation FROM contract_leases")?;
    let leases = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    let mut query = tx.prepare("SELECT caller,call_key,request,response FROM contract_calls")?;
    let calls = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(caller, key, request, response)| {
            Ok((
                serde_json::from_str(&caller)?,
                key,
                serde_json::from_str(&request)?,
                serde_json::from_str(&response)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut query = tx.prepare("SELECT caller,call_key,fact FROM contract_facts")?;
    let facts = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(caller, key, fact)| Ok((serde_json::from_str(&caller)?, key, fact)))
        .collect::<Result<Vec<_>>>()?;
    let retired: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='contract_retirements')", [], |r| r.get(0))?;
    Ok(LegacySnapshot {
        scope,
        endpoint,
        root: root.into(),
        source: json!({"format":"M3-contract-v4","application_id":app,"user_version":version,"incarnation":incarnation,"database_sha256":source_digest}),
        seats: records(&tx, "contract_seats")?,
        operations: records(&tx, "contract_operations")?,
        generations,
        instances,
        leases,
        retirements: if retired {
            records(&tx, "contract_retirements")?
        } else {
            Vec::new()
        },
        connections: records(&tx, "contract_connections")?,
        calls,
        facts,
        peers: records(&tx, "contract_framework_peers")?,
        result_route,
    })
}
