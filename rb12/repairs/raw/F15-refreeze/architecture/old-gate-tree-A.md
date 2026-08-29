# Rust code abstract tree

- module: `/Volumes/nvme/tmp/f15-f14-parent-312d19a3/crates/team-agent/tests/e2e/cases/gate_hole_061_red.rs`
- direction: `A`
- output_lines: 1147
- estimated_tokens: 14911
- token_basis: `13 tokens/output line`
- macro_policy: preserve definitions/invocations; do not expand generated AST

## `cases/gate_hole_061_red.rs`

```rust
//! 0.5.61 gate-coverage-hole RED: the documented fresh-team command and first
//! message/result loop belong to the existing CLI E2E hard smoke.
//!
//! Requirement anchors:
//! - `skills/team-agent/SKILL.md` "Minimal Copy-Paste Team" and "Commands"
//! - F1 one-entry startup / stable team identity
//! - F4 end-to-end delivery truth and unique recipient
//! - F10 requirement-to-RED and anti-vacuous controls
//!
//! Reanchor:
//! - `collect` must contain both the spawned fake-worker's original message-scoped result and
//!   the independent stdio MCP supplemental result; the latter cannot mask loss of the former.
//! - command coverage is an honest A-covered / B-declared-gap / C-last-resort-exemption catalog.
//!   Each A entry explicitly declares one source/test function, literal invocation, binding,
//!   literal assertion node, behavior operand, and executable negative twin. The authority
//!   resolves those declarations through Rust token trees (never substring/character-position
//!   inference), requires every node exactly once, and admits A only when the normal mapped case
//!   passes while the one-field twin fails at that declared assertion. Cross-entry admission also
//!   checks observable twin-discrimination cells; nested diagnostic/control subtrees do not become
//!   behavior evidence merely by containing the binding tokens. Provider launchers retain their
//!   additional hermetic PATH shim and exact argv-log obligations.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::framework::*;
use crate::support::source_walker::source_tree;
use rusqlite::Connection;
use serde_json::Value;

const LNCH_CASE: &str = "lnch_001_quick_start_basic";
const SEND_CASE: &str = "send_001_delivers_to_fake_worker";
const COVERAGE_MANIFEST: &str = "skills/team-agent/command-coverage.json";
const TOOTH_3B_VERDICT_ARTIFACT: &str = "gate-hole-061-tooth-3b-verdict.json";
const TWIN_OBSERVATION_NONCE_ENV: &str = "TEAM_AGENT_TWIN_OBSERVATION_NONCE";
const TWIN_OBSERVATION_PREFIX: &str = "TEAM_AGENT_TWIN-OBSERVATION-V1";
const TWIN_OBSERVATION_SCENARIO_ENV: &str = "TEAM_AGENT_TWIN_OBSERVATION_SCENARIO";

#[test]
fn tooth_1_existing_launch_smoke_runs_documented_quick_start_verbatim() {
}

#[test]
fn tooth_2_existing_send_smoke_proves_worker_receive_report_and_collect() {
}

#[test]
fn tooth_3a_every_skill_command_is_recorded_losslessly() {
}

#[ignore = "red-by-design: pending contract, tracked in private backlog"]
#[test]
fn tooth_3b_three_bucket_claims_are_honest_and_launcher_safe() {
}

fn evaluate_tooth_3b() -> Result<TwinDiscriminationOutcome, String> {
}

#[test]
fn gate_hole_negative_twin_execution_canary_case() {
}

#[ignore = "red-by-design: pending contract, tracked in private backlog"]
#[test]
fn gate_hole_twin_discrimination_canary_case() {
}

#[derive(Debug)]
struct MessageTruth {
    recipient: String,
    status: String,
    delivered_at: Option<String>,
}

#[derive(Debug)]
struct ResultTruth {
    result_id: String,
    task_id: String,
    agent_id: String,
    summary: String,
}

impl MessageTruth {
    fn delivered(&self) -> bool {
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageManifest {
    schema_version: String,
    #[serde(default)]
    authority: Option<CoverageAuthority>,
    commands: Vec<CoverageEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageAuthority {
    kind: String,
    handbook: HandbookAuthority,
    live_help: LiveHelpAuthority,
    compact_skill_smoke: CompactSkillSmokeAuthority,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HandbookAuthority {
    path: String,
    start_marker: String,
    end_marker: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveHelpAuthority {
    argv: Vec<String>,
    source: String,
    root_command_policy: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactSkillSmokeAuthority {
    path: String,
    policy: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "bucket", rename_all = "snake_case", deny_unknown_fields)]
enum CoverageEntry {
    Covered {
        command: String,
        #[serde(default)]
        cases: Vec<String>,
        #[serde(default)]
        evidence: Option<CoveredEvidenceDeclaration>,
        #[serde(default)]
        launcher_shim_evidence: Option<LauncherShimEvidence>,
    },
    DeclaredGap {
        command: String,
        #[serde(default)]
        covered: Option<bool>,
        #[serde(default)]
        owner: String,
        #[serde(default)]
        plan: String,
    },
    Exempt {
        command: String,
        #[serde(default)]
        category: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        owner: String,
        #[serde(default)]
        shim_or_isolation_infeasible: Option<bool>,
    },
}

impl CoverageEntry {
    fn command(&self) -> &str {
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LauncherShimEvidence {
    case: String,
    provider: String,
    argv_log_binding: String,
    cli_result_binding: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoveredEvidenceDeclaration {
    case: String,
    source_file: String,
    invocation: InvocationDeclaration,
    binding: BindingDeclaration,
    assertion: AssertionDeclaration,
    negative_twin: NegativeTwinDeclaration,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InvocationDeclaration {
    runner: String,
    line: usize,
    documented_argv: Vec<String>,
    literal_argv: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingDeclaration {
    name: String,
    line: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionDeclaration {
    macro_name: String,
    line: usize,
    operand: String,
    behavior_fact: String,
    failure_marker: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NegativeTwinDeclaration {
    env_key: String,
    env_value: String,
    operation: String,
    remove_literal: String,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RustTokenKind {
    Ident(String),
    StringLiteral(String),
    Number(String),
    CharLiteral,
    Punct(char),
    Group {
        delimiter: char,
        tokens: Vec<RustToken>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustToken {
    kind: RustTokenKind,
    line: usize,
}

#[derive(Debug, Clone)]
struct FunctionNode {
    body: Vec<RustToken>,
}

#[derive(Debug, Clone)]
struct RunTaCall {
    runner: String,
    binding: Option<String>,
    binding_line: Option<usize>,
    line: usize,
    argv: Vec<String>,
    has_path_override: bool,
}

#[derive(Debug, Clone)]
struct AssertionNode {
    name: String,
    line: usize,
    path_qualified: bool,
    arguments: Vec<RustToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateTerminalStatus {
    Green,
    Pending,
    Red,
}

impl GateTerminalStatus {
    fn as_str(self) -> &'static str {
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct PendingTwinCell {
    row: usize,
    column: usize,
    outcome: String,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TwinDiscriminationOutcome {
    Complete,
    Pending(Vec<PendingTwinCell>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwinObservationResult {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GateVerdict {
    status: GateTerminalStatus,
    reason: Option<String>,
    pending_cells: Vec<PendingTwinCell>,
}

impl GateVerdict {
    fn from_validation(result: Result<TwinDiscriminationOutcome, String>) -> Self {
    }

    fn red(reason: String) -> Self {
    }

    fn allows_success(&self) -> bool {
    }
}

fn verdict_artifact_path() -> PathBuf {
}

fn gate_verdict_value(verdict: &GateVerdict) -> Value {
}

fn write_gate_verdict(verdict: &GateVerdict) -> Result<PathBuf, String> {
}

fn finalize_gate_verdict(verdict: GateVerdict) {
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
}

fn documented_fake_team(tag: &str, agent_id: &str) -> TestWorkspace {
}

fn run_worker_mcp_report_result(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: &str,
    summary: &str,
) {
}

fn shutdown(ws: &TestWorkspace) {
}

fn message_truth(workspace: &Path, message_id: &str) -> Option<MessageTruth> {
}

fn result_truth_for_message(workspace: &Path, message_id: &str) -> Option<ResultTruth> {
}

fn collected_rows_include(rows: &[Value], expected: &ResultTruth, scope: &str) -> bool {
}

fn worker_delivery_truth_matches(expected: &str, truth: &MessageTruth) -> bool {
}

fn assert_worker_truth_negative_canary() {
}

fn assert_original_result_collect_negative_canary() {
}

fn assert_documented_argv_canary() {
}

fn assert_command_extractor_canary() {
}

fn assert_coverage_closed_world_canary() {
}

fn assert_three_bucket_validator_canary() {
}

fn canary_evidence(
    macro_name: &str,
    operand: &str,
    behavior_fact: &str,
    assertion_line: usize,
) -> CoveredEvidenceDeclaration {
}

fn covered_canary_entry(command: &str, evidence: CoveredEvidenceDeclaration) -> CoverageEntry {
}

fn assert_global_evidence_identity_canary() {
}

fn canary_source(assertion: &str) -> String {
}

fn canary_source_with_invocation(invocation: &str, assertion: &str) -> String {
}

fn canary_source_with_extra(assertion: &str, extra: &str) -> String {
}

fn assert_syntax_failure(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
    signature: &str,
) {
}

fn assert_red_then_restored_green(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    invalid: &str,
    restored: &str,
    signature: &str,
) {
}

fn assert_failure_signature(result: Result<(), String>, signature: &str) {
}

fn load_coverage_manifest(tooth: &str) -> CoverageManifest {
}

fn unique_manifest_commands(manifest: &CoverageManifest) -> Result<BTreeSet<String>, String> {
}

fn validate_assertion_twin_pair_uniqueness(manifest: &CoverageManifest) -> Result<(), String> {
}

fn validate_bucket_fields(manifest: &CoverageManifest) -> Result<(), String> {
}

fn validate_expected_bucket_totals(
    manifest: &CoverageManifest,
    expected_a: usize,
    expected_b: usize,
    expected_c: usize,
) -> Result<(), String> {
}

fn validate_covered_evidence(
    manifest: &CoverageManifest,
    _e2e_tests: &str,
) -> Result<TwinDiscriminationOutcome, String> {
}

fn validate_covered_case_registration(manifest: &CoverageManifest) -> Result<(), String> {
}

fn validate_launcher_shim_evidence(
    command: &str,
    provider: &str,
    covered: &CoveredEvidenceDeclaration,
    evidence: &LauncherShimEvidence,
    source: &str,
) -> Result<(), String> {
}

fn validate_no_unshimmed_launcher_calls(
    manifest: &CoverageManifest,
    e2e_tests: &str,
) -> Result<(), String> {
}

fn command_set_drift(documented: &BTreeSet<String>, listed: &BTreeSet<String>) -> Option<String> {
}

fn extract_normative_handbook_commands(
    markdown: &str,
    start_marker: &str,
    end_marker: &str,
) -> Result<BTreeSet<String>, String> {
}

fn exact_live_help_roots(
    authority: &LiveHelpAuthority,
    normative: &BTreeSet<String>,
    handbook_commands: &BTreeSet<String>,
) -> BTreeSet<String> {
}

fn extract_team_agent_commands(markdown: &str) -> BTreeSet<String> {
}

fn normalize_team_agent_command(raw: &str) -> Option<String> {
}

fn documented_command_matches_argv(command: &str, actual: &[String]) -> bool {
}

fn launcher_provider_from_command(command: &str) -> Option<String> {
}

fn launcher_provider_from_argv(argv: &[String]) -> Option<&str> {
}

fn shell_tokens(command: &str) -> Option<Vec<String>> {
}

fn validate_declared_evidence_syntax(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
}

fn validate_declared_evidence_nodes(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
}

fn validate_negative_twin_hook(
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
}

fn node_count_failure(kind: &str, count: usize, _declared: &str) -> String {
}

fn covered_evidence_entries(
    manifest: &CoverageManifest,
) -> Vec<(&str, &CoveredEvidenceDeclaration)> {
}

fn same_assertion_node(
    left: &CoveredEvidenceDeclaration,
    right: &CoveredEvidenceDeclaration,
) -> bool {
}

fn declared_assertion_is_top_level(evidence: &CoveredEvidenceDeclaration) -> Result<bool, String> {
}

fn emit_twin_cell_raw(row: usize, column: usize, outcome: &str, observed: &str) {
}

fn parse_twin_observations(
    observed: &str,
    expected_nonce: &str,
    expected_cells: &BTreeSet<String>,
) -> Result<BTreeMap<String, TwinObservationResult>, String> {
}

fn complete_twin_observations(
    observed: &str,
    expected_nonce: &str,
    expected_cells: &BTreeSet<String>,
) -> Result<Option<BTreeMap<String, TwinObservationResult>>, String> {
}

fn twin_observation_nonce(row: usize) -> String {
}

fn validate_observable_twin_discrimination(
    manifest: &CoverageManifest,
) -> Result<TwinDiscriminationOutcome, String> {
}

fn assert_mapped_case_positive(source_file: &str, case: &str) -> Result<(), String> {
}

fn assert_mapped_case_negative_twin(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
) -> Result<(), String> {
}

fn run_negative_twin_at_declared_assertion(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
) -> Result<String, String> {
}

fn run_negative_twin_raw(evidence: &CoveredEvidenceDeclaration) -> Result<(bool, String), String> {
}

fn run_twin_observation_raw(
    evidence: &CoveredEvidenceDeclaration,
    nonce: &str,
) -> Result<String, String> {
}

fn observed_at_declared_assertion(evidence: &CoveredEvidenceDeclaration, observed: &str) -> bool {
}

fn run_exact_e2e_case(
    source_file: &str,
    case: &str,
    twin: Option<(&str, &str)>,
) -> Result<std::process::Output, String> {
}

fn run_exact_e2e_case_with_envs(
    source_file: &str,
    case: &str,
    envs: &[(&str, &str)],
) -> Result<std::process::Output, String> {
}

fn assert_negative_twin_executor_canary() {
}

fn assert_twin_observation_protocol_canary() {
}

fn assert_twin_discrimination_canary() {
}

fn rust_syntax_tokens(source: &str) -> Result<Vec<RustToken>, String> {
}

struct RustTokenParser<'a> {
    source: &'a str,
    index: usize,
    line: usize,
}

impl RustTokenParser<'_> {
    fn parse_sequence(&mut self, closing: Option<u8>) -> Result<Vec<RustToken>, String> {
    }

    fn skip_space_and_comments(&mut self) -> Result<(), String> {
    }

    fn take_raw_string(&mut self) -> Result<Option<String>, String> {
    }

    fn take_string(&mut self) -> Result<String, String> {
    }

    fn take_char_literal(&mut self) -> Result<bool, String> {
    }
}

fn decode_rust_string(raw: &str) -> Result<String, String> {
}

fn test_function_nodes(tokens: &[RustToken], case: &str) -> Vec<FunctionNode> {
}

fn collect_test_function_nodes(
    tokens: &[RustToken],
    case: &str,
    functions: &mut Vec<FunctionNode>,
) {
}

fn has_test_attribute(tokens: &[RustToken], fn_index: usize) -> bool {
}

fn module_declaration_count(tokens: &[RustToken], module: &str) -> usize {
}

fn run_ta_calls(tokens: &[RustToken]) -> Vec<RunTaCall> {
}

fn collect_run_ta_calls(tokens: &[RustToken], calls: &mut Vec<RunTaCall>) {
}

fn binding_before(tokens: &[RustToken], call_index: usize) -> Option<(String, usize)> {
}

fn binding_nodes(tokens: &[RustToken]) -> Vec<(String, usize)> {
}

fn literal_argv(tokens: &[RustToken]) -> Option<Vec<String>> {
}

fn literal_string_array(tokens: &[RustToken]) -> Option<Vec<String>> {
}

fn assertion_nodes(tokens: &[RustToken]) -> Vec<AssertionNode> {
}

fn top_level_assertion_nodes(tokens: &[RustToken]) -> Vec<AssertionNode> {
}

fn collect_assertion_nodes_at_current_level(
    tokens: &[RustToken],
    assertions: &mut Vec<AssertionNode>,
) {
}

fn collect_assertion_nodes(tokens: &[RustToken], assertions: &mut Vec<AssertionNode>) {
}

fn assertion_operands(assertion: &AssertionNode) -> Option<Vec<&[RustToken]>> {
}

fn behavior_fact_in_tokens(
    tokens: &[RustToken],
    binding: &str,
    fact: &str,
) -> Result<bool, String> {
}

fn behavior_fact_required_by_expression(
    tokens: &[RustToken],
    binding: &str,
    fact: &str,
    rejected_contexts: &mut BTreeSet<&'static str>,
) -> bool {
}

fn raw_behavior_fact_in_tokens(tokens: &[RustToken], binding: &str, fact: &str) -> bool {
}

fn logical_or_branches(tokens: &[RustToken]) -> Vec<&[RustToken]> {
}

fn is_expression_start(tokens: &[RustToken], index: usize) -> bool {
}

fn nested_macro_at(tokens: &[RustToken], index: usize) -> Option<(&str, &[RustToken])> {
}

fn nested_macro_context(name: &str) -> &'static str {
}

fn closure_body_at(tokens: &[RustToken], index: usize) -> Option<&[RustToken]> {
}

fn negative_twin_hook_lines(
    tokens: &[RustToken],
    evidence: &CoveredEvidenceDeclaration,
) -> Vec<usize> {
}

fn collect_negative_twin_hook_lines(
    tokens: &[RustToken],
    evidence: &CoveredEvidenceDeclaration,
    lines: &mut Vec<usize>,
) {
}

fn negative_twin_condition_matches(tokens: &[RustToken], twin: &NegativeTwinDeclaration) -> bool {
}

fn negative_twin_body_matches(tokens: &[RustToken], evidence: &CoveredEvidenceDeclaration) -> bool {
}

fn syntax_atoms(tokens: &[RustToken]) -> Vec<String> {
}

fn binding_assigned_named_call(tokens: &[RustToken], binding: &str, function: &str) -> bool {
}

fn identifier_in_tokens(tokens: &[RustToken], identifier: &str) -> bool {
}

fn string_literal_in_tokens(tokens: &[RustToken], expected: &str) -> bool {
}

fn string_literal_contains(tokens: &[RustToken], expected: &str) -> bool {
}

fn token_ident(token: Option<&RustToken>) -> Option<&str> {
}

fn token_group(token: Option<&RustToken>, delimiter: char) -> Option<&[RustToken]> {
}

fn repo_root() -> PathBuf {
}
```

# Adjacent module contracts

## `framework.rs`

```rust
//! E2E framework: TestWorkspace + run_ta + assert helpers + FakeProvider
//! support + state injection + wait_for. Zero external test-CLI deps —
//! framework uses `std::process::Command`, `serde_json`, and the existing
//! `team-agent` binary built by `cargo test`.
//!
//! ---
//! purpose: Hermetic macOS E2E fixture ownership and durable delivery timeout evidence
//! contract:
//!   provides:
//!     - name: TestWorkspace
//!       what: Owns exact coordinator and tmux resources and reaps them on drop
//!     - name: wait_for_delivery_or_panic
//!       what: Persists message, coordinator, event, and physical target facts before timeout panic
//!   depends:
//!     - crate::platform::process
//!     - sqlite messages/events store
//!     - tmux per-team endpoint
//! boundary:
//!   - Test-only fixture and evidence surface; no delivery product behavior
//! maturity: wired
//! ---
//!
//! All test helpers panic on programmer error (wrong binary path, write
//! failure on a temp dir we own) and return `Result` / printable diagnostics
//! when the SUT misbehaves.

/// A self-cleaning workspace directory under `/private/tmp` (preferred on
/// macOS so it survives `/tmp -> /private/tmp` symlink resolution that some
/// runtime paths do) or `std::env::temp_dir()` elsewhere. The directory is
/// removed on `Drop` unless `TEAM_AGENT_KEEP_TEST_TMP=1` is set.
pub struct TestWorkspace {
    pub(crate) path: PathBuf,
    pub(crate) ta_binary: Mutex<Option<PathBuf>>,
    /// 0.5.43 debt-sweep (§6.1): exact test-owned tmux sockets to
    /// clean at Drop. Populated by `register_owned_tmux_socket`. Drop
    /// runs `tmux -S <sock> kill-server` on each (never a host scan)
    /// BEFORE the workspace directory removal (verified by RED
    /// `e2e_workspace_drop_cleans_exact_tmux_before_removing_workspace`).
    pub(crate) owned_tmux_sockets: Mutex<Vec<PathBuf>>,
}

impl TestWorkspace {
/// Create a workspace tagged `e2e-<tag>-<pid>-<seq>`. The tag becomes part
    /// of the dirname — pass a short label per test so kept dirs are easy to
    /// identify.
    pub fn new(tag: &str) -> Self {
    }

/// 0.5.43 debt-sweep (§6.1): register a test-owned tmux socket for
    /// exact Drop cleanup. Never a host-wide scan — the ledger only
    /// contains sockets THIS fixture created.
    pub fn register_owned_tmux_socket(&self, socket: &Path) {
    }

pub fn path(&self) -> &Path {
    }

pub(crate) fn record_ta_binary(&self, path: &Path) {
    }

/// Write a minimal TEAM.md + agents/<id>.md tree that uses
    /// `provider: fake` (no subscription, no real provider binary). Returns
    /// `self` for chaining.
    pub fn with_fake_spec(self, agent_ids: &[&str]) -> Self {
    }

/// Path to `.team/runtime/state.json` (may not exist before quick-start).
    pub fn state_json_path(&self) -> PathBuf {
    }

pub fn events_jsonl_path(&self) -> PathBuf {
    }

/// Read state.json as a serde_json::Value. Panics if the file doesn't
    /// exist or is malformed — those are framework-level failures, not SUT
    /// misbehaviour.
    pub fn read_state(&self) -> Value {
    }

/// Inject (or override) a top-level field in state.json. Panics if
    /// state.json doesn't exist yet — callers must run at least one CLI
    /// command that creates it first, or call `seed_state()` instead.
    pub fn inject_state(&self, top_level_key: &str, value: Value) {
    }

/// Inject an agent-level field. `agent_id` must already exist in
    /// `state.agents`.
    pub fn inject_agent_field(&self, agent_id: &str, field: &str, value: Value) {
    }

/// Seed state.json from scratch (creates `.team/runtime/`). Use when a
    /// test wants to start from an arbitrary pre-built state without running
    /// quick-start first.
    pub fn seed_state(&self, state: Value) {
    }

pub fn write_state_value(&self, state: Value) {
    }

pub fn mutate_state<F>(&self, f: F)
    where
        F: FnOnce(&mut Value),
    {
    }

pub fn mutate_agent_everywhere<F>(&self, agent_id: &str, mut f: F)
    where
        F: FnMut(&mut serde_json::Map<String, Value>),
    {
    }
}

impl Drop for TestWorkspace {
fn drop(&mut self) {
    }
}

impl TestWorkspace {
pub(crate) fn coordinator_pid_file(&self) -> PathBuf {
    }

pub(crate) fn pid_is_owned_coordinator(&self, pid: u32) -> bool {
    }

pub(crate) fn command_is_owned_coordinator(&self, command: &str) -> bool {
    }
}

pub(crate) fn read_pid(path: &Path) -> Option<u32> {
}

pub(crate) fn normalize_existing_path(path: &Path) -> PathBuf {
}

pub(crate) fn pid_is_running(pid: u32) -> bool {
}

pub(crate) fn wait_until_pid_exits(pid: u32, timeout: Duration) -> bool {
}

/// Structured result of a `team-agent <cmd>` invocation.
#[derive(Debug, Clone)]
pub struct TaResult {
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl TaResult {
/// Parse stdout as JSON. Panics with a diagnostic showing argv + stderr
    /// when stdout is not valid JSON — the caller should EXPECT JSON because
    /// every E2E test should pass `--json`.
    pub fn json(&self) -> Value {
    }

pub fn is_success(&self) -> bool {
    }
}

/// Locate the freshly-built `team-agent` binary that Cargo produces for this
/// test. Cargo sets `CARGO_BIN_EXE_team-agent` for integration tests of the
/// owning crate, which is the recommended modern API.
pub(crate) fn ta_binary() -> PathBuf {
}

/// Run a `team-agent` CLI invocation. The first arg is the subcommand. The
/// framework does NOT auto-inject `--workspace` or `--json` — pass them
/// explicitly so test intent is visible.
pub fn run_ta(ws: &TestWorkspace, args: &[&str]) -> TaResult {
}

/// Like `run_ta` but lets the caller splice extra env entries (key/value
/// pairs). Per-command env keeps parallel tests safe — never set process
/// globals via `std::env::set_var`.
pub fn run_ta_env(ws: &TestWorkspace, args: &[&str], extra_env: &[(&str, &str)]) -> TaResult {
}

/// Assert that a JSON pointer (RFC 6901) resolves to an expected `Value`.
/// Use slash-paths: `assert_json_field(&out, "/ok", &json!(true))`.
#[track_caller]
pub fn assert_json_field(actual: &Value, pointer: &str, expected: &Value) {
}

#[track_caller]
pub fn assert_json_field_eq_bool(actual: &Value, pointer: &str, expected: bool) {
}

#[track_caller]
pub fn assert_json_field_eq_str(actual: &Value, pointer: &str, expected: &str) {
}

#[track_caller]
pub fn assert_json_field_present(actual: &Value, pointer: &str) {
}

/// Return `true` if a tmux session with `name` exists on the default tmux
/// socket. Returns `false` if `tmux` is not installed or no server is
/// running (those are not assertion failures — the SUT manages its own
/// server).
pub fn tmux_session_exists(name: &str) -> bool {
}

/// Return `true` if a tmux session exists on a *specific* socket (the SUT
/// uses per-team sockets like `ta-<hash>`). Pass the full `-S` or `-L`
/// argument as recorded by SUT (e.g. via state.tmux_socket).
pub fn tmux_session_exists_on_socket(socket_arg: &str, name: &str) -> bool {
}

pub fn tmux_windows_on_socket(socket_arg: &str, name: &str) -> Vec<String> {
}

#[track_caller]
pub fn assert_tmux_session_absent(name: &str) {
}

#[track_caller]
pub fn assert_tmux_session_present(name: &str) {
}

/// Kill a tmux session on the default socket, ignoring errors (used in test
/// teardown belt-and-suspenders to clean up residual leader sessions a test
/// may have left if it crashed before completing shutdown).
pub fn tmux_kill_session_quiet(name: &str) {
}

#[track_caller]
pub fn assert_file_exists(path: &Path) {
}

#[track_caller]
pub fn assert_file_absent(path: &Path) {
}

/// Assert that a UTF-8 file contains a substring.
#[track_caller]
pub fn assert_file_contains(path: &Path, needle: &str) {
}

/// Poll `predicate` until it returns `true` or `timeout` elapses. Returns
/// `true` if the predicate succeeded, `false` if it timed out. `poll_every`
/// caps how often the predicate is re-evaluated.
pub fn wait_for<F: FnMut() -> bool>(
    mut predicate: F,
    timeout: Duration,
    poll_every: Duration,
) -> bool {
}

#[track_caller]
pub fn wait_for_or_panic<F: FnMut() -> bool>(description: &str, predicate: F, timeout: Duration) {
}

/// Poll a delivery predicate and preserve enough evidence to classify a
/// timeout after `TestWorkspace` teardown removes the live fixture. The
/// snapshot intentionally lives outside the workspace and is keyed by the
/// exact message id; it is therefore safe for the controlled concurrent lane.
/// ---
/// purpose: Poll a message delivery obligation and retain timeout evidence
/// contract:
///   provides:
///     - name: wait_for_delivery_or_panic
///       what: Binds a delivery timeout to its row, coordinator, event, and physical target facts
///   depends:
///     - TestWorkspace-owned runtime files and tmux endpoint
/// boundary:
///   - Test-only timeout evidence; it does not alter delivery state
/// maturity: wired
/// ---
pub fn wait_for_delivery_or_panic<F: FnMut() -> bool>(
    ws: &TestWorkspace,
    message_id: &str,
    recipient: &str,
    description: &str,
    mut predicate: F,
    timeout: Duration,
) {
}

/// Convenience: was the quick-start good enough for E2E to continue? Returns
/// true if the JSON shows the team was launched, even when the leader receiver
/// is unbound (which is normal under `cargo test` where no $TMUX is exported
/// — the framework strips TMUX to keep test isolation, so leader pane binding
/// fails by design). Tests that specifically need a bound leader_receiver
/// should attach manually or assert on `qs.json()["status"]` themselves.
pub fn quick_start_launched(result: &TaResult) -> bool {
}

/// Some tests want a workspace that has gone through quick-start so state.json
/// + events.jsonl exist with realistic shape. This helper does that and
/// returns the result for further inspection.
pub fn quick_start_fake(ws: &TestWorkspace, team_id: &str) -> TaResult {
}

/// Sanitize team_id into the tmux session name as the runtime does:
/// session = `team-<team_id>` (lowercased, no transformation needed for our
/// safe ids). Use this everywhere to avoid scattering the convention.
pub fn worker_session_name(team_id: &str) -> String {
}

/// Read `tmux_socket` from state.json (full path) and check whether a session
/// exists on that specific socket. Returns `false` if state.json doesn't yet
/// have a socket entry — callers should treat that as "no live tmux yet".
pub fn tmux_session_exists_for_workspace(ws: &TestWorkspace, name: &str) -> bool {
}

pub fn tmux_windows_for_workspace(ws: &TestWorkspace, name: &str) -> Vec<String> {
}

pub fn tmux_window_exists_for_workspace(ws: &TestWorkspace, session: &str, window: &str) -> bool {
}

#[track_caller]
pub fn state_agent<'a>(state: &'a Value, agent_id: &str) -> &'a Value {
}

pub fn state_has_agent(state: &Value, agent_id: &str) -> bool {
}

/// Misc helper: collect a tag → value map of all keys present at the top
/// level of state.json. Useful for diagnostic prints in failing tests.
pub fn state_top_level_keys(state: &Value) -> BTreeMap<String, String> {
}

#[cfg(all(test, unix))]
mod containment_tests {}
```

## `support/mod.rs`

```rust
pub mod source_walker;

pub mod topology_issue_ids;
```

## `support/source_walker.rs`

```rust
pub fn source_tree(rels: &[&str]) -> String {
}
```

## `support/topology_issue_ids.rs`

```rust
pub const WORKER_PANE_BINDING_STALE: &str = "worker_pane_binding_stale";

pub const TMUX_ENDPOINT_SOCKET_CONFLICT: &str = "tmux_endpoint_socket_conflict";

pub const LEADER_RECEIVER_SOCKET_MISMATCH: &str = "leader_receiver_socket_mismatch";

pub const ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET: &str = "orphan_team_session_on_ignored_socket";

pub const TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET: &str =
    "team_session_missing_on_canonical_socket";

pub const RECENT_COORDINATOR_SESSION_MISSING: &str = "recent_coordinator_session_missing";

pub const LEADER_PANE_ID_COLLIDES_WITH_AGENT: &str = "LeaderPaneIdCollidesWithAgent";
```
