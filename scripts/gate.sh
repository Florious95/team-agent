#!/usr/bin/env bash
# Consolidated architecture and governance gates. Rule explanations and
# examples live in ../DESIGN-RULES.md; keep this command and that index linked.

set -u

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

ROOT_ID=$(printf '%s' "$ROOT" | cksum | awk '{print $1}')
TARGET_DIR=${CARGO_TARGET_DIR:-/Volumes/nvme/tmp/team-agent-gate-suite-$ROOT_ID}
FAILURES=0
LINE_COUNT_BASELINE=86

run_gate() {
  local name=$1
  shift
  local log
  log=$(mktemp "${TMPDIR:-/tmp}/team-agent-gate.XXXXXX")
  if "$@" >"$log" 2>&1; then
    printf '[PASS] %s\n' "$name"
  else
    local rc=$?
    printf '[FAIL] %s (exit=%s)\n' "$name" "$rc"
    sed 's/^/  /' "$log"
    FAILURES=$((FAILURES + 1))
  fi
  rm -f "$log"
}

expect_one() {
  local pattern=$1
  local file=$2
  local matches
  matches=$(grep -rnE "$pattern" "$file") || return $?
  [[ $(printf '%s\n' "$matches" | wc -l | tr -d ' ') == 1 ]] || {
    printf 'expected exactly one match for %s in %s\n%s\n' "$pattern" "$file" "$matches"
    return 1
  }
}

expect_none() {
  local pattern=$1
  shift
  local output rc
  output=$(grep -rnE "$pattern" "$@" 2>&1)
  rc=$?
  if [[ $rc == 1 ]]; then
    return 0
  fi
  if [[ $rc == 0 ]]; then
    printf 'forbidden dependency matched:\n%s\n' "$output"
    return 1
  fi
  printf 'grep failed (exit=%s):\n%s\n' "$rc" "$output"
  return "$rc"
}

check_c1_send() {
  expect_one '^use super::persist::persist_resolved_target;$' crates/team-agent/src/cli/send/resolve.rs &&
    expect_one '^use super::presentation::delivery_outcome_json;$' crates/team-agent/src/cli/send/persist.rs &&
    expect_none '^use super::(resolve|persist)|^use super::\*' \
      crates/team-agent/src/cli/send/persist.rs \
      crates/team-agent/src/cli/send/presentation.rs &&
    expect_none '^use super::presentation|^use super::\*' \
      crates/team-agent/src/cli/send/resolve.rs
}

check_c2_status() {
  expect_one '^use super::store::\{' crates/team-agent/src/cli/status_port/snapshot.rs &&
    expect_none 'RuntimeSnapshot|latest_result_summaries|^use super::(snapshot|store)|^use super::\*' \
      crates/team-agent/src/cli/status_port/format.rs &&
    expect_none 'RuntimeSnapshot::assemble|^use super::(snapshot|format)|^use super::\*' \
      crates/team-agent/src/cli/status_port/store.rs &&
    expect_none 'super::format|format::|format_agent_status' \
      crates/team-agent/src/cli/status_port/snapshot.rs
}

check_c3_launch() {
  expect_one '^use super::approval::detect_dangerous_approval;$' \
    crates/team-agent/src/lifecycle/launch/spec_state.rs &&
    expect_one '^use super::spec_state::\{has_positive_caller_leader_env, spec_agent_values\};$' \
      crates/team-agent/src/lifecycle/launch/state_projection.rs &&
    expect_none '^use super::spec_state|^use super::state_projection|^use super::\*' \
      crates/team-agent/src/lifecycle/launch/approval.rs \
      crates/team-agent/src/lifecycle/launch/spec_state.rs &&
    expect_none '^(pub([^ ]*)? )?use (approval|spec_state|state_projection)::\*' \
      crates/team-agent/src/lifecycle/launch.rs
}

cargo_test() {
  env -u TMUX -u TMUX_PANE env -i \
    HOME="$HOME" \
    PATH="$PATH" \
    USER="${USER:-}" \
    LANG="${LANG:-en_US.UTF-8}" \
    TEAM_AGENT_TEST_TMP=/Volumes/nvme/tmp \
    CARGO_TARGET_DIR="$TARGET_DIR" \
    cargo test -p team-agent "$@"
}

check_state_ratchet() {
  cargo_test --test s1a_governance_hardening \
    hard1_ratchet_pins_current_snapshot_and_monotone_baseline -- --exact &&
    cargo_test --test s1a_governance_hardening \
      hard2_direct_save_family_cannot_be_aliased_or_imported_outside_authority -- --exact
}

check_raw_read_surface() {
  cargo_test --test s1a_governance_hardening \
    hard3_runtime_state_surface_files_are_enumerated -- --exact &&
    cargo_test --test s1a_read_route_guard \
      raw_read_facade_is_the_single_non_migrating_read_ingress -- --exact
}

check_source_guards() {
  cargo_test --test e7_host_leader_registry_contract &&
    cargo_test --test transport_factory_compact_status_guard &&
    cargo_test --test current_alias_single_source_guard &&
    cargo_test --test runtime_mcp_approval_red \
      running_agent_state_persists_effective_policy_schema_and_single_helper_across_spawn_paths \
      -- --exact
}

check_line_count_ledger() {
  local output summary over_limit
  output=$(python3 tools/check_line_count_gate.py \
    --root crates/team-agent/src \
    --glob '*.rs' \
    --max-lines 500 \
    --require-empty-temporary-debt) || {
    printf '%s\n' "$output"
    return 1
  }
  summary=$(printf '%s\n' "$output" | tail -n 1)
  if [[ $summary =~ over-limit:\ ([0-9]+)\ files ]]; then
    over_limit=${BASH_REMATCH[1]}
  else
    printf 'could not parse line-count summary:\n%s\n' "$summary"
    return 1
  fi
  if (( over_limit > LINE_COUNT_BASELINE )); then
    printf 'line-count debt grew: %s > frozen baseline %s\n' \
      "$over_limit" "$LINE_COUNT_BASELINE"
    return 1
  fi

  local file lines
  while IFS= read -r file; do
    lines=$(wc -l <"$file" | tr -d ' ')
    if (( lines > 500 )); then
      printf 'split target exceeded 500 lines: %s (%s)\n' "$file" "$lines"
      return 1
    fi
  done < <(
    printf '%s\n' \
      crates/team-agent/src/cli/send.rs \
      crates/team-agent/src/cli/status_port.rs \
      crates/team-agent/src/lifecycle/launch.rs
    find \
      crates/team-agent/src/cli/send \
      crates/team-agent/src/cli/status_port \
      crates/team-agent/src/lifecycle/launch \
      -maxdepth 1 -type f -name '*.rs' -print | sort
  )
  printf '%s\n' "$summary"
}

check_r6_static_guard() {
  cargo_test --test test_isolation_escape_contract \
    r6_static_guard_rejects_dangerous_tests_without_hermetic_boundary -- --exact
}

run_gate reverse-edge-c1-send check_c1_send
run_gate reverse-edge-c2-status check_c2_status
run_gate reverse-edge-c3-launch check_c3_launch
run_gate state-write-ratchet check_state_ratchet
run_gate raw-read-scanner check_raw_read_surface
run_gate composite-source-guards check_source_guards
run_gate line-count-ledger check_line_count_ledger
run_gate r6-hermetic-static-guard check_r6_static_guard

if (( FAILURES > 0 )); then
  printf '[FAIL] total (%s gate(s) failed)\n' "$FAILURES"
  exit 1
fi

printf '[PASS] total (8 gates)\n'
