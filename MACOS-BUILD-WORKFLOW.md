# macOS arm64 candidate build

`.github/workflows/build-macos-candidate.yml` is a manual-only workflow for producing a reviewable `team-agent` candidate on a native `macos-14` arm64 runner. It does not publish packages, create tags/releases, install anything globally, or invoke the npm publish workflow.

## Dispatch inputs

Both inputs are required and must be the complete 40-character hexadecimal values:

- `source_sha`: exact commit to check out and build;
- `expected_tree`: exact tree SHA expected at that commit.

The job checks out only `source_sha` with credentials disabled, then fails unless `HEAD`, `HEAD^{tree}`, and the clean checkout match the requested identities. For the frozen claim candidate, the values are:

```text
source_sha=d80131d5f87643082227d37ab9aaf760a19228a5
expected_tree=af5575b76df7893403881cbfdec3a0a4234edae6
```

After this workflow is registered on the repository default branch, a dispatch can be made with the GitHub UI or equivalent API request. For example:

```sh
gh workflow run build-macos-candidate.yml \
  --repo Florious95/team-agent \
  --ref main \
  -f source_sha=d80131d5f87643082227d37ab9aaf760a19228a5 \
  -f expected_tree=af5575b76df7893403881cbfdec3a0a4234edae6
```

## Candidate artifact

The job uses Rust `1.95.0` and runs exactly:

```text
cargo build -p team-agent --bin team-agent --release --target aarch64-apple-darwin --locked
```

It verifies the host and Rust target are arm64, checks the output is exactly a Mach-O arm64 executable, runs `team-agent --version`, and uploads a seven-day artifact containing:

- `team-agent-aarch64-apple-darwin`;
- `team-agent-macos-arm64.tar.gz` (executable mode preserved);
- `SOURCE.txt` and `BUILD.txt` provenance;
- `CLI-VERSION.txt`;
- `SHA256SUMS`.

A workflow file on a non-default branch is not dispatchable through GitHub's normal workflow-dispatch surface. Until this PR is merged to the default branch, no candidate run is attempted; the workflow has no automatic trigger.
