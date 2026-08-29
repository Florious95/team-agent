# Rust code abstract tree

- module: `/Volumes/nvme/tmp/f15-f14-parent-312d19a3/crates/team-agent/tests/e2e/cases/gate_hole_061_red.rs`
- direction: `C`
- output_lines: 34
- estimated_tokens: 442
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
```
