# Gap 46 Schema Migration Contract

## Fixture

`tests/contracts/legacy_team_db_fixture.py` is the deterministic legacy
database synthesizer. It uses raw `CREATE TABLE` statements, not the current
schema module, to create a 0.1.4-style `team.db` where `owner_team_id` exists
at the physical end of `results`, `messages`, `scheduled_events`, and
`agent_health`.

## Prevention

- `initialize_schema()` is the live entry gate. It must check physical column
  layout before `_ensure_table_columns` or any other read/write path can treat
  the database as current.
- Layout drift is detected by physical column order from `pragma table_info`,
  not only by column presence.
- Drift rebuild runs in one atomic transaction. A crash between copy and rename
  leaves either the complete pre-state or the complete post-state, never a
  mixed or half-rebuilt schema.
- Before any destructive rewrite, Team Agent writes
  `.team/runtime/team.db.pre-migration-<utc>-from-v<N>.bak`.
- The rebuild is generic across every `_ensure_table_columns` managed table;
  the hotfix core requires `results`, `messages`, `scheduled_events`, and
  `agent_health`.
- Versioned migrations are chained from `pragma user_version` to
  `SCHEMA_VERSION` through a migration registry.
- Managed-table DB access uses explicit column lists and named row access.
  `SELECT *` and positional row access are forbidden for `message_store/`
  managed-table paths.

## Detection

- `team-agent doctor` without `--fix-schema` is read-only. It reports layout
  diffs, version state, and the recommended repair without mutating `team.db`.
- `team-agent doctor --fix-schema` runs the same rebuild logic as
  `initialize_schema`, writes the mandatory backup first, and refuses active
  DB locks with a structured blocked result.
- Every rebuild emits `schema.layout_rebuild` with table name, from/to layouts,
  backup path, and `row_count_before == row_count_after`. Blocked paths emit
  `schema.layout_rebuild_blocked` with a closed reason.

## Non-Goals

Doctor fixes schema layout only. It does not promote `update_state` records or
perform result-store policy recovery.
