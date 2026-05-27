# spark-reviewer findings — Gap 32 idle/takeover redesign

- Target commit: `c6e8bb823b6202ea3031decb5371737da534f5c5`
- Scope reviewed: new provider-neutral redesign modules only (no coordinator wiring observed in runtime entrypoints)

## Findings

### HIGH: New idle/takeover subsystem is not connected to coordinator tick path (design intent not active)
- **Files/lines**:
  - `src/team_agent/coordinator/lifecycle.py:263,286`
  - `src/team_agent/coordinator/lifecycle.py:286`
  - `src/team_agent/messaging/idle_alerts.py:347` (old reminder body text still emitted)
  - `src/team_agent/idle_takeover.py:1`
  - `src/team_agent/idle_predicate.py:1`
- **Issue**: `coordinator.tick` still imports/executes `team_agent.messaging.idle_alerts.detect_idle_fallbacks` and never imports/uses `idle_takeover.evaluate_takeover_reminder`/`classify_provider_turn_state`. The redesigned modules are present but dead code relative to runtime behavior. Whole `C10` and redesigned `C1-C11` flow therefore cannot take effect.
- **Suggested fix shape**:
  - Replace (or gate behind rollout flag) coordinator invocation at `coordinator.lifecycle` to call the new idle facade;
  - keep old path behind explicit compatibility branch until acceptance proves parity;
  - ensure a single source emits reminders and suppression from one path.

### MEDIUM: C4 process identity guard can misclassify open turns as alive when identity fields are incomplete
- **Files/lines**:
  - `src/team_agent/provider_state/common.py:101-142`
  - `src/team_agent/provider_state/common.py:128-139`
- **Issue**: `process_is_live` returns `alive=True` for `process=None` or non-dict and only flags PID replacement if both identity fields (and/or both pid fields) are present and unequal. If caller supplies only partial identity (common in cross-version/permission-limited reads), a replaced PID can be treated as valid working work, so `turn_open` never degrades to `abnormal` and idle detection can be suppressed.
- **Suggested fix shape**: tighten fallback behavior to return `False` (or explicit `unknown`) when mandatory identity fields are missing for an existing `turn_open`; require verified identity match rather than assuming alive.

### MEDIUM: C8 dedupe collapses valid repeats when `turn_id` is absent/None
- **Files/lines**:
  - `src/team_agent/abnormal_track.py:47-63`
- **Issue**: dedupe key is `f"{signature}\x00{turn_id}"`; if readers emit `turn_id=None` (possible for some approval/error records), all records with same signature de-duplicate globally, dropping repeated actionable faults.
- **Suggested fix shape**: include a stable fallback dimension (e.g. event sequence/index or provider+provider-specific turn hash + timestamp bucket) whenever `turn_id` is missing.

### MEDIUM: C11 suspend-window subtraction does not normalize interval overlap/duplicates
- **Files/lines**:
  - `src/team_agent/idle_predicate.py:128-142`
- **Issue**: every suspend interval is subtracted independently. Overlapping or duplicated `(start, end)` windows can be double-counted, shrinking elapsed time more than intended and over-delaying reminders.
- **Suggested fix shape**: normalize/merge suspend intervals (sort + coalesce overlaps) before summing; ignore zero/negative/invalid intervals.

### MEDIUM: New provider adapter checklist is outside committed source because `docs/` is globally gitignored
- **Files/lines**:
  - `.gitignore:5`
  - `docs/adding-a-provider-idle-adapter.md:1`
  - `src/team_agent/provider_state/registry.py:18-95`
- **Issue**: operational checklist and onboarding contract for adding providers is not part of repo history and lives in ignored path, so discoverability/auditability for this slice is not guaranteed. This also breaks “新增-CLI checklist完整性” expectation.
- **Suggested fix shape**: move checklist into versioned source (e.g. `src/team_agent/provider_state/README.md`) and link from registry/CLI doc location.
