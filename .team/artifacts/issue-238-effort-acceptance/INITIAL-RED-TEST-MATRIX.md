# Issue #238 Initial RED test matrix

Product baseline under test: `fd8a9890ce4890fd49619ff2aed8e30e7e483f6f`.

The tester patch keeps `ultra` as a runtime wire literal so the frozen product
still compiles the test and fails for the missing behavior rather than failing
at test compilation.

| Requirement | Test surface | Expected baseline result |
|---|---|---|
| R1 | `tests/issue238_codex_effort_contract.rs` serde/trim/strict parser | RED: `ultra` is not parsed |
| R2 | nine-provider × six-effort resolver matrix | RED: missing `ultra`, Codex `max` admission/reason text stale |
| R3 | compiler role/team precedence and Pi isolation | RED: Codex `max`/`ultra` and Pi `ultra` contract mismatch |
| R4 | direct `validate_spec` acceptance/rejection | RED: new literals/reasons not accepted |
| R5 | Codex fresh/resume/fork `CommandPlan` profile passthrough | RED: `ultra` cannot reach plan |
| R6 | lifecycle YAML + persisted JSON spawn/recovery helpers | RED: persisted `ultra` cannot resolve |
| R7 | Pi leader `--thinking` provider-semantic boundary | RED: `ultra` reports unknown instead of Codex-only reason |
| R8 | ignored-provider and Cursor/Claude protection paths | protected legacy paths stay explicit; new Codex paths RED |

The developer track receives only the frozen taskbook and this acceptance
contract, never the test source or patch.

Remote execution receipts are recorded beside this file after Grok Bot runs;
the authoritative status is the remote `CommandExit`, not an accepted/queued
launcher response.
