# Provider model discovery

`team-agent models` is a read-only, fuzzy-searchable view of a provider's native model catalog. It helps find a copyable model ID; it does not choose a model, certify subscription access, or change launch validation.

## Supported catalog sources

| `--provider` value | Native source | Canonical result ID |
|---|---|---|
| `pi` | `pi --list-models` | Pi's exact `provider/model` ID |
| `cursor_agent` | `agent --list-models` | Cursor catalog ID |
| `codex` | `codex debug models` | Codex `slug` |
| `claude` | Claude CLI SDK stream-json `initialize` response | selector `resolvedModel` |
| `claude_code` | same Claude source as `claude` | selector `resolvedModel`; output provider remains `claude_code` |

Catalogs are fetched per invocation, with one native command, a 10-second deadline, and a 1 MiB output bound. Team Agent does not cache results, maintain static model allowlists, perform login, call a model-completion endpoint, or treat a visible catalog row as proof of inference entitlement. Provider-native login/account checks remain the provider CLI's responsibility.

Codex rows are validated as a whole. Only `visibility: list` rows are displayed; `hide` and `none` rows are not. Claude selectors are grouped only when their source `resolvedModel` strings are byte-identical. Selector `value`s are exposed as aliases for search; the canonical `resolvedModel` remains the returned model ID.

## Search and errors

Search is case-insensitive substring matching over provider, vendor, exact ID, display name, and source-provided aliases. Query text is split by whitespace: every token must match at least one field, and results retain native catalog order. Search does not normalize punctuation or invent aliases. Exact IDs remain exact and copyable in output.

A valid catalog with no search matches is a successful empty result; rerun without a query to list the catalog. An unavailable, timed-out, failed, oversized, empty, malformed, duplicate, or unsupported catalog is an error. No partial catalog is shown on failure. Catalog lookup does not weaken exact model validation at launch or alter provider defaults/reasoning effort.

Examples:

```sh
team-agent models --provider codex --search 'GPT luna'
team-agent models --provider claude --search 'opus 1m'
team-agent models --provider claude_code --json
```

## Adding a provider catalog

New providers must follow the shared `provider::model_catalog` contract rather than adding a CLI-specific matcher:

1. Make an explicit catalog-capability decision for every `Provider` enum variant; unsupported providers remain explicitly unsupported.
2. Use exactly one PATH-first native catalog observation. Bound execution by the common deadline and output size; do not construct shell command strings, probe credentials, retry, or silently fall back to stale/static data.
3. Parse the complete native response into `ModelRecord { provider, vendor, id, display_name, default, aliases }`. Preserve canonical IDs byte-for-byte; only include aliases that the source actually provides.
4. Validate the full source before filtering. Reject malformed schemas, missing required fields, duplicate source identities, and empty catalogs instead of returning partial results.
5. Reuse the shared token-AND, field-OR matcher and preserve source order. Never fold ambiguous candidates into one guessed choice.
6. Keep discovery separate from launch/selection validation. Do not turn fuzzy matches into accepted launch IDs, add a static whitelist, change defaults, or infer provider access from catalog visibility.
7. Add tests for exact-ID fidelity, catalog provenance, token/order/case properties, alias handling, ambiguity, no-match versus source failure, and fail-closed malformed/duplicate data; document the official source and protocol fields.

Update this page when registering a new catalog source. See [operator guidance](team-agent-operator.md) for provider setup and credentials.
