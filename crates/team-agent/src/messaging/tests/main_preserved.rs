use super::*;

// ── collect --result-file INGEST SEMANTIC (golden results.py:58-73) ──────────────────────────────
// Golden `collect(result_file=…)` INDEPENDENTLY INGESTS a standalone result: a valid result_envelope_v1
// is `store.add_result(envelope)`'d (results.py:73) regardless of any in-flight delivery, then the
// collection loop collects it when its task_id is a known task (state.tasks) OR message-scoped (msg_+
// matching message). NO live in-flight task is required at ingest time. rt-host-b @ c262bf7 saw a
// VALID envelope collect to exit-1 / empty output / NOT in the
// results table — i.e. the --result-file ingest path was a no-op (results.rs once did
// `let _ = (result_file, …)`). This pins the happy path: a valid envelope for a KNOWN task must be
// ingested into the results table AND collected, with ok=true. (Completes the previously-deferred
// collected_results golden — the fixture is built from the real compiler + state persistence.)
