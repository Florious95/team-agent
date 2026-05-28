# Bug 058 Contract: WSL `/mnt/c` npx bin-link failure

## Fixture

The real user failure is captured verbatim in
`tests/fixtures/bug_058/wsl_mnt_c_npx_not_found.txt`.

The package metadata is already valid: `package.json` declares
`bin.team-agent-installer = npm/install.mjs`, `npm/install.mjs` has a node
shebang, and the published files include `npm/`. Bug 058 is not a package
manifest problem; it is a user-visible npm/npx environment failure where npm
does not expose the installed bin shim in WSL under `/mnt/c/Users/<user>` when a
project-level `.npmrc` contains `prefix=...`.

## Contract

1. Documentation must give a visible recovery path for Windows + WSL users.
   The install documentation shipped in the npm package (`README.md`) must have
   a dedicated `Windows + WSL` section that names all observable facts:
   `/mnt/c/Users/<user>`, project-level `.npmrc`, `prefix`, npm's
   `config prefix cannot be changed from project config` warning, and
   `team-agent-installer: not found`. The section must tell users to move the
   setting to `~/.npmrc` or delete the project-level `prefix` line, and to
   retry from `cd ~`.

2. Installation must diagnose the missing bin shim before users see the
   cryptic shell error. `package.json` must declare a `postinstall` script that
   runs a committed npm-side self-check script. The check must verify whether
   `team-agent-installer` is discoverable after npm installation.

3. The postinstall diagnostic must be structured and actionable. On missing bin
   shim it must print these sections:
   `ERROR: team-agent-installer bin not on PATH after npm install.`,
   `ACTION:`, and `LOG:`. The action text must mention the WSL `/mnt/c`
   `.npmrc prefix` case, moving/removing the project-level prefix, and retrying
   from `cd ~`.

4. The diagnostic must be mechanically testable without real WSL. The
   postinstall script must support deterministic acceptance mode via
   `TEAM_AGENT_INSTALLER_SELF_CHECK_ONLY=1`; in that mode it uses `INIT_CWD`,
   `PATH`, and the filesystem visible to the test process. A temp project path
   containing `.npmrc` with `prefix=...` and a `PATH` that lacks
   `team-agent-installer` must make the script fail closed with the structured
   diagnostic above.

5. The implementation must not hide or rewrite npm's policy warning and must
   not attempt to make project-level `prefix` valid. No package script may call
   `npm config set prefix`, delete `.npmrc`, or silently edit npm config.

## Acceptance

`tests/test_bug_058_red.py` is the executable contract. It is intentionally
RED before implementation.
