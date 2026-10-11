# Kiro CLI discovery receipt

Research scope: read-only environment/help discovery and official documentation retrieval for the Kiro contract landing taskbook. This is not product acceptance.

## Source identity

- Canonical source: `/Volumes/nvme/Builds/team-agent-build-link/team-agent-candidate-2252d369`
- HEAD and `golden-contract-spec^{commit}`: `508f37bf8884f6c4c6288ae00bab2938d9300ed3`
- Tree: `951d1023f46b2a4f8ce95d030359ac7ba2885ad3`
- Contract: `docs/reference/provider-contract-spec.md`
- Contract SHA256: `f9d940086dd45f9a624fcbc6bdd680ff7e65fea0319ca8bf0e3ba90bb383cddd`
- `git status --short --untracked-files=no`: empty at the 2026-10-08T09:08:36Z check.

## Native environment: observations are time-scoped

1. Initial lookup during this task found none of `kiro-cli`, `kiro`, `q`, `qchat` in PATH. A names-only scan of `/Applications`, `~/Applications`, `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` found no Kiro/Amazon-Q candidates. The initial check was reported to leader; its exact timestamp was not persisted. It is NOT the current availability conclusion.
2. Recheck at **2026-10-08T09:08:36Z** found `/opt/homebrew/bin/kiro-cli`, a symlink to `/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli`. The other three command names still did not resolve. This worker did not install anything.
3. Resolved binary: 74,723,360 bytes, universal Mach-O x86_64 + arm64. SHA256: `dee3f382fc8f6734fe505b815d786ed634ba67a258ba34986eca50f4f0b5fc22`.
4. Application bundle metadata, not CLI version output: `CFBundleShortVersionString=2.28.0`, `CFBundleVersion=20261006.001055`, `CFBundleIdentifier=com.amazon.codewhisperer`, `CFBundleExecutable=kiro_cli_desktop`.
5. Bounded help/version invocations started at **2026-10-08T09:08:56Z**. `--version`, `--help`, `chat --help`, `agent --help`, `mcp --help` each exceeded the 15-second observation limit. Exact argv/durations/hash are in [native-help-receipts.json](native-help-receipts.json). Python `subprocess.run(timeout=15)` terminated only its directly spawned child on timeout. These are harness TIMEOUT results, **not native exit code 124, not an authentication diagnosis, and not proof that those flags are unsupported**. No authoritative command output/exit receipt was obtained; partial output was not retained and remains UNKNOWN.
6. A subsequent process check used only `pid,ppid,etime,stat,comm`. It showed a Kiro process older than these attempts; ownership was not established, so it was neither signalled nor inspected further. No causal claim about the help timeouts is made.

Current conclusion: **binary present; executable version/help/engine capability unverified; interactive key timing, prompt grammar, authentication readiness, catalog, MCP return, session operations and exit codes NOT-RUN**. Installation/OS gate/parent-process interaction are hypotheses, not established causes. No further launch retries are authorized by this receipt.

## Official references

[fetch-receipts.json](fetch-receipts.json) binds 19 successful HTTPS fetches to exact URL, final URL, byte count and SHA256. These files describe documentation at fetch time, not the capabilities of the observed binary or account. Markdown content was fetched as data, never executed.

Important distinctions:

- CLI application version and agent harness V2/V3 are different dimensions. V3 is described as an opt-in early release. UI engine TUI/classic is a third dimension; V3 does not support classic.
- `chat --list-models --format json`, `--effort`, exact `--resume-id`, custom-agent prompt files and stdio MCP are documented; native help/catalog remain unverified here.
- Terminal UI docs describe a startup risk acknowledgement for `--trust-all-tools`. This is not a second Enter for each message.
- Enter while busy can steer or queue; cancellation can auto-send queued content. Ctrl+C documentation is context-dependent (exit vs cancel/redirect); never use it as an unconditional cleanup key.
- The public model pages list GPT-5.6 Luna and model-dependent effort. They do not establish account availability, the installed CLI's catalog or a native Luna 6 slug.
- `/session-id`, exact-ID resume, `/chat save/load` and `/rewind N` are documented. V2/V3 sessions are incompatible, cloud/local are distinct, and rewind does not roll back files.
- The configuration reference says `includeMcpJson=false` excludes both user and workspace MCP files; V3 summaries mention only workspace. The installed version must confirm isolation.
- Exit code 4 may be emitted after a default agent has already performed actions. It cannot be converted to NoEffect or an automatic replay instruction.

## Safety and unexecuted work

No Cargo/rustc/build/product tests; no credential/database/session-store reads; no login, settings writes, native interactive/model invocation, global installation, old-Provider edits, production-code changes, merge or release. Official docs' example shell commands were not executed. Native authentication storage path supplied by leader is treated as a protected boundary, not a discovery target.
