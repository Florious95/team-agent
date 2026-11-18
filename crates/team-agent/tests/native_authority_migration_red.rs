//! 0.6.2 D1 cutover contract (dev-astra): `team-agent migrate` (also reached by
//! mutating start/stop preflight) moves a QUIESCENT legacy scope to the private
//! state root exactly once; a live scope is deferred. Reads and `send` never
//! initiate cutover. Durable two-file publication: an intent under the new root,
//! then the old binding is retired to framework-binding.pre-state-home.json,
//! then the new binding is published. An intent is never authority; after old
//! retirement and before publication reads refuse; migrate again rolls forward.
#![cfg(unix)]
use std::fs;
use std::path::Path;

#[path = "support/d1_support.rs"]
mod d1;
use d1::*;

fn state_root(state_home: &Path, team: &NativeTeam) -> std::path::PathBuf {
    state_home.join("team-agent/contract").join(team.scope.as_str())
}

fn migrate(team: &NativeTeam, state_home: &Path) -> serde_json::Value {
    json_of(&cli(&team.ws, state_home, &["migrate", "--team", "alpha"]))
}

/// RED on 0.6.1 (its M3 `migrate` rewrites a live binding); GREEN under D1: a live
/// legacy scope is deferred. The old binding stays authoritative,
/// nothing is archived, no new authoritative binding appears and send still works.
#[test]
fn live_legacy_scope_defers_migration_and_keeps_delivering() {
    let base = scratch("mig-live");
    let state_home = base.join("state");
    let team = native_team(|ws, scope| vec![legacy_root(ws, scope)]);
    let legacy = legacy_root(&team.ws, &team.scope);
    let before = fs::read(legacy.join(BINDING)).unwrap();
    let outcome = migrate(&team, &state_home);
    let after = fs::read(legacy.join(BINDING)).ok();
    let archived = legacy.join(ARCHIVE).exists();
    let published = state_root(&state_home, &team).join(BINDING).exists();
    let receipt = send(&team.ws, &state_home, "live scope still delivers after deferred migrate");
    drop(team);
    let _ = fs::remove_dir_all(&base);
    assert_eq!(after.as_deref(), Some(before.as_slice()), "live legacy binding changed: {outcome}");
    assert!(!archived && !published, "live scope was cut over: {outcome}");
    assert_eq!(receipt["delivery"], "physical", "{receipt}");
}

/// RED on 0.6.1: a stopped (quiescent) legacy scope is cut over exactly once.
#[test]
fn stopped_legacy_scope_migrates_exactly_once() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("mig-once");
    let state_home = base.join("state");
    let team = quiescent_team(|ws, scope| vec![legacy_root(ws, scope)]);
    let legacy = legacy_root(&team.ws, &team.scope);
    let original = fs::read(legacy.join(BINDING)).unwrap();
    let first = migrate(&team, &state_home);
    let new_root = state_root(&state_home, &team);
    let published = fs::read(new_root.join(BINDING));
    let archive = fs::read(legacy.join(ARCHIVE));
    let legacy_live = legacy.join(BINDING).exists();
    let modes = (fs::metadata(&new_root).map(|m| m.permissions().mode() & 0o777).ok(),
        fs::metadata(new_root.join(BINDING)).map(|m| m.permissions().mode() & 0o777).ok());
    let second = migrate(&team, &state_home);
    let published_again = fs::read(new_root.join(BINDING)).ok();
    let archive_again = fs::read(legacy.join(ARCHIVE)).ok();
    let archives = fs::read_dir(&legacy).map(|entries| entries.filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("pre-state-home")).count()).unwrap_or(0);
    drop(team);
    let _ = fs::remove_dir_all(&base);
    let published = published.unwrap_or_else(|error| panic!("no state-home binding after migrate ({error}): {first}"));
    assert_eq!(archive.as_deref().ok(), Some(original.as_slice()), "old binding not retired byte-for-byte once: {first}");
    assert!(!legacy_live, "two live bindings after cutover: {first}");
    let (old, new): (serde_json::Value, serde_json::Value) =
        (serde_json::from_slice(&original).unwrap(), serde_json::from_slice(&published).unwrap());
    for key in ["version", "workspace", "team", "scope", "endpoint"] {
        assert_eq!(old[key], new[key], "migrated binding changed {key}");
    }
    assert_eq!(modes, (Some(0o700), Some(0o600)), "state-home authority is not private");
    assert_eq!(published_again.as_deref(), Some(published.as_slice()), "second migrate rewrote the binding: {second}");
    assert_eq!(archive_again.as_deref(), Some(original.as_slice()), "second migrate touched the archive: {second}");
    assert_eq!(archives, 1, "archive must be retained exactly once");
}

/// RED on 0.6.1: interruption after source retirement and before publication.
/// The state is rebuilt from the genuine files of a real, successful cutover;
/// no authorization is fabricated. Reads refuse; migrate again rolls forward.
#[test]
fn interrupted_cutover_after_source_retirement_refuses_reads_then_rolls_forward() {
    let base = scratch("mig-crash");
    let state_home = base.join("state");
    let team = quiescent_team(|ws, scope| vec![legacy_root(ws, scope)]);
    let legacy = legacy_root(&team.ws, &team.scope);
    let first = migrate(&team, &state_home);
    let new_root = state_root(&state_home, &team);
    let published = fs::read(new_root.join(BINDING))
        .unwrap_or_else(|error| panic!("precondition: a real cutover must succeed first ({error}): {first}"));
    assert!(new_root.join(INTENT).exists(),
        "precondition: the durable intent must be retained (or reproducible) to rebuild the interrupted state: {first}");
    // Interrupted between "old retired" and "new published": intent + archive, no live binding.
    fs::remove_file(new_root.join(BINDING)).unwrap();
    let read = send(&team.ws, &state_home, "must not deliver mid-cutover");
    let resumed = migrate(&team, &state_home);
    let rolled = fs::read(new_root.join(BINDING)).ok();
    let legacy_live = legacy.join(BINDING).exists();
    drop(team);
    let _ = fs::remove_dir_all(&base);
    assert_ne!(read["delivery"], "physical", "a read used an intent or archive as authority: {read}");
    assert!(read["delivery_error"].as_str().is_some_and(|e| !e.is_empty()) || read["ok"] == false, "{read}");
    assert_eq!(rolled.as_deref(), Some(published.as_slice()), "migrate did not roll forward to the prepared binding: {resumed}");
    assert!(!legacy_live, "roll-forward resurrected the retired source: {resumed}");
}

/// RED on 0.6.1: before source retirement the old binding stays authoritative even
/// when a genuine intent exists; the intent is never read as authority.
#[test]
fn intent_before_source_retirement_is_not_authority() {
    let base = scratch("mig-intent");
    let state_home = base.join("state");
    let team = quiescent_team(|ws, scope| vec![legacy_root(ws, scope)]);
    let legacy = legacy_root(&team.ws, &team.scope);
    let first = migrate(&team, &state_home);
    let new_root = state_root(&state_home, &team);
    assert!(new_root.join(BINDING).exists() && new_root.join(INTENT).exists(),
        "precondition: a real cutover with a retained intent: {first}");
    // Rebuild "intent written, source not yet retired" from the genuine files.
    fs::remove_file(new_root.join(BINDING)).unwrap();
    fs::rename(legacy.join(ARCHIVE), legacy.join(BINDING)).unwrap();
    let resumed = migrate(&team, &state_home);
    let completed = new_root.join(BINDING).exists() && legacy.join(ARCHIVE).exists() && !legacy.join(BINDING).exists();
    drop(team);
    let _ = fs::remove_dir_all(&base);
    assert!(completed, "migrate did not complete the prepared cutover: {resumed}");
}
