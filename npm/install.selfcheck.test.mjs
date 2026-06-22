import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  doctorSelfCheckLine,
  doctorSelfCheckVerdict,
  parseTeamAgentVersion,
  readInstallManifest,
  repairPathShadowingTeamAgentCommands,
  resolveInstallBinDir,
  verifyInstalledTeamAgentOnPath,
  writeInstallManifest,
} from "./install.mjs";

test("empty doctor workspace is ok for installer self-check", () => {
  const verdict = doctorSelfCheckVerdict(
    JSON.stringify({
      ok: false,
      error: "workspace has no Team Agent spec or runtime context",
      tmux: { installed: true },
      mcp: { server_command: "/x/team_orchestrator" },
    }),
    { status: 1 },
  );

  assert.equal(verdict.kind, "ok");
  assert.deepEqual(verdict.blockers, []);
});

test("real doctor blockers remain blockers", () => {
  const verdict = doctorSelfCheckVerdict(
    JSON.stringify({
      ok: false,
      tmux: { installed: false },
      mcp: { server_command: null },
    }),
    { status: 1 },
  );

  assert.equal(verdict.kind, "blockers");
  assert.ok(verdict.blockers.some((blocker) => blocker.includes("tmux")));
});

test("unparseable or killed doctor self-check is advisory without blocker wording", () => {
  for (const [doctorBody, spawnMeta] of [
    ["", { status: 1 }],
    ["not json", { status: 1 }],
    [JSON.stringify({ ok: false }), { signal: "SIGTERM" }],
  ]) {
    const verdict = doctorSelfCheckVerdict(doctorBody, spawnMeta);
    assert.equal(verdict.kind, "advisory");
    assert.doesNotMatch(doctorSelfCheckLine(verdict), /has blockers/);
  }
});

test("install bin resolver chooses writable on-path dir and skips node version dirs", () => {
  const root = tempRoot("path-choice");
  const versionBin = path.join(root, ".nvm", "versions", "node", "v22.0.0", "bin");
  const npxBin = path.join(root, ".npm", "_npx", "abc123", "node_modules", ".bin");
  const pathBin = path.join(root, "homebrew", "bin");
  fs.mkdirSync(versionBin, { recursive: true });
  fs.mkdirSync(npxBin, { recursive: true });
  fs.mkdirSync(pathBin, { recursive: true });

  const resolved = resolveInstallBinDir({
    env: { PATH: [npxBin, versionBin, pathBin].join(path.delimiter), SHELL: "/bin/zsh" },
    home: root,
  });

  assert.equal(resolved.binDir, pathBin);
  assert.equal(resolved.kind, "path");
  assert.equal(resolved.readyNow, true);
  assert.equal(fs.existsSync(path.join(root, ".zshrc")), false);
});

test("install bin resolver falls back to shell rc idempotently", () => {
  const root = tempRoot("path-rc");

  const first = resolveInstallBinDir({
    env: { PATH: "/usr/bin:/bin", SHELL: "/bin/zsh" },
    home: root,
  });
  const second = resolveInstallBinDir({
    env: { PATH: "/usr/bin:/bin", SHELL: "/bin/zsh" },
    home: root,
  });

  assert.equal(first.binDir, path.join(root, ".local", "bin"));
  assert.equal(first.kind, "shell_rc");
  assert.equal(first.readyNow, false);
  assert.equal(second.binDir, first.binDir);
  const zshrc = fs.readFileSync(path.join(root, ".zshrc"), "utf8");
  assert.equal((zshrc.match(/# team-agent PATH \(E48\)/g) || []).length, 1);
});

test("install bin resolver skips writable path dir with foreign wrapper", () => {
  const root = tempRoot("foreign-wrapper");
  const foreignBin = path.join(root, "foreign", "bin");
  const pathBin = path.join(root, "stable", "bin");
  fs.mkdirSync(foreignBin, { recursive: true });
  fs.mkdirSync(pathBin, { recursive: true });
  fs.writeFileSync(path.join(foreignBin, "team-agent"), "#!/bin/sh\necho foreign\n", { mode: 0o755 });

  const resolved = resolveInstallBinDir({
    env: { PATH: [foreignBin, pathBin].join(path.delimiter), SHELL: "/bin/zsh" },
    home: root,
  });

  assert.equal(resolved.binDir, pathBin);
  assert.equal(resolved.readyNow, true);
});

test("post-install repair updates higher-priority stale local team-agent binary", () => {
  const root = tempRoot("path-shadow");
  const localBin = path.join(root, ".local", "bin");
  const installBin = path.join(root, ".hermes", "bin");
  const runtimeBinary = path.join(root, "runtime", "0.3.test", "bin", "team-agent");
  fs.mkdirSync(localBin, { recursive: true });
  fs.mkdirSync(installBin, { recursive: true });
  fs.mkdirSync(path.dirname(runtimeBinary), { recursive: true });
  fs.writeFileSync(runtimeBinary, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  fs.writeFileSync(path.join(localBin, "team-agent"), "#!/bin/sh\necho old team-agent\n", { mode: 0o755 });
  fs.writeFileSync(path.join(installBin, "team-agent"), "#!/bin/sh\necho installed\n", { mode: 0o755 });
  const logs = [];

  const repairs = repairPathShadowingTeamAgentCommands({
    env: { PATH: [localBin, installBin].join(path.delimiter) },
    home: root,
    binDir: installBin,
    runtimeBinary,
    log: (line) => logs.push(line),
  });

  assert.deepEqual(repairs.map((repair) => repair.file), [path.join(localBin, "team-agent")]);
  assert.ok(logs.some((line) => line.includes("path-shadow: scanning")));
  assert.ok(logs.some((line) => line.includes(`found ${path.join(localBin, "team-agent")}`)));
  assert.ok(logs.some((line) => line.includes(`updated ${path.join(localBin, "team-agent")}`)));
  const repaired = fs.readFileSync(path.join(localBin, "team-agent"), "utf8");
  assert.match(repaired, /team-agent installer wrapper/);
  assert.match(repaired, new RegExp(escapeRegExp(runtimeBinary)));
});

test("post-install repair probes home local bin even when npm PATH misses it", () => {
  const root = tempRoot("path-shadow-home-local");
  const localBin = path.join(root, ".local", "bin");
  const installBin = path.join(root, ".hermes", "bin");
  const runtimeBinary = path.join(root, "runtime", "0.3.test", "bin", "team-agent");
  fs.mkdirSync(localBin, { recursive: true });
  fs.mkdirSync(installBin, { recursive: true });
  fs.mkdirSync(path.dirname(runtimeBinary), { recursive: true });
  fs.writeFileSync(runtimeBinary, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  fs.writeFileSync(path.join(localBin, "team-agent"), "#!/bin/sh\necho old team-agent\n", { mode: 0o755 });
  fs.writeFileSync(path.join(installBin, "team-agent"), "#!/bin/sh\necho installed\n", { mode: 0o755 });
  const logs = [];

  const repairs = repairPathShadowingTeamAgentCommands({
    env: { PATH: installBin },
    home: root,
    binDir: installBin,
    runtimeBinary,
    log: (line) => logs.push(line),
  });

  assert.deepEqual(repairs.map((repair) => repair.file), [path.join(localBin, "team-agent")]);
  assert.ok(logs.some((line) => line.includes(`found ${path.join(localBin, "team-agent")} source=home-local-bin`)));
  const repaired = fs.readFileSync(path.join(localBin, "team-agent"), "utf8");
  assert.match(repaired, /team-agent installer wrapper/);
  assert.match(repaired, new RegExp(escapeRegExp(runtimeBinary)));
});

test("post-install repair leaves lower-priority team-agent binary untouched", () => {
  const root = tempRoot("path-shadow-after");
  const installBin = path.join(root, ".hermes", "bin");
  const laterBin = path.join(root, "later", "bin");
  const runtimeBinary = path.join(root, "runtime", "0.3.test", "bin", "team-agent");
  fs.mkdirSync(installBin, { recursive: true });
  fs.mkdirSync(laterBin, { recursive: true });
  fs.mkdirSync(path.dirname(runtimeBinary), { recursive: true });
  fs.writeFileSync(runtimeBinary, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  fs.writeFileSync(path.join(installBin, "team-agent"), "#!/bin/sh\necho installed\n", { mode: 0o755 });
  fs.writeFileSync(path.join(laterBin, "team-agent"), "#!/bin/sh\necho later old\n", { mode: 0o755 });

  const repairs = repairPathShadowingTeamAgentCommands({
    env: { PATH: [installBin, laterBin].join(path.delimiter) },
    home: root,
    binDir: installBin,
    runtimeBinary,
  });

  assert.deepEqual(repairs, []);
  assert.equal(fs.readFileSync(path.join(laterBin, "team-agent"), "utf8"), "#!/bin/sh\necho later old\n");
});

test("post-install version check verifies the actual team-agent resolved on PATH", () => {
  const root = tempRoot("version-check");
  const binDir = path.join(root, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  writeVersionScript(path.join(binDir, "team-agent"), "1.2.3");

  const check = verifyInstalledTeamAgentOnPath({
    env: { PATH: [binDir, process.env.PATH || ""].join(path.delimiter) },
    expectedVersion: "1.2.3",
  });

  assert.equal(check.entry, path.join(binDir, "team-agent"));
  assert.equal(check.version, "1.2.3");
});

test("post-install version check fails when PATH still resolves an old team-agent", () => {
  const root = tempRoot("version-mismatch");
  const binDir = path.join(root, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  writeVersionScript(path.join(binDir, "team-agent"), "0.3.36");

  assert.throws(
    () =>
      verifyInstalledTeamAgentOnPath({
        env: { PATH: [binDir, process.env.PATH || ""].join(path.delimiter) },
        expectedVersion: "0.3.37",
      }),
    /PATH resolves team-agent.*0\.3\.36.*installed 0\.3\.37/,
  );
});

test("team-agent version parser accepts installer-safe output shapes", () => {
  assert.equal(parseTeamAgentVersion("team-agent 1.2.3\n"), "1.2.3");
  assert.equal(parseTeamAgentVersion("1.2.3\n"), "1.2.3");
  assert.equal(parseTeamAgentVersion("usage: nope\n"), null);
});

test("install manifest persists selected bin dir for later commands", () => {
  const root = tempRoot("manifest");
  const runtimeRoot = path.join(root, "runtime");
  const binDir = path.join(root, "bin");

  writeInstallManifest(runtimeRoot, { binDir, version: "test-version" });
  const manifest = readInstallManifest(runtimeRoot);

  assert.equal(manifest.binDir, binDir);
  assert.equal(manifest.version, "test-version");
});

// Regression guard for the codesign/SIGKILL fix: the install flow MUST stage
// the runtime under `.<version>.<pid>.tmp` and use `fs.renameSync(tmp, dest)`
// to swap it into place. A direct `fs.copyFileSync` into the destination bin
// path is the bad path that triggers the macOS code-sign cache failure on
// in-place overwrite. The Darwin ad-hoc codesign helper must also be wired
// in between staging and the swap.
test("installer stages under temp dir and codesigns before atomic rename", () => {
  const source = fs.readFileSync(
    path.join(path.dirname(new URL(import.meta.url).pathname), "install.mjs"),
    "utf8",
  );
  // Temp-dir staging pattern: `.${version}.${process.pid}.tmp` under runtimeRoot.
  assert.match(
    source,
    /\.\$\{version\}\.\$\{process\.pid\}\.tmp/,
    "install must stage under .<version>.<pid>.tmp (not overwrite dest in place)",
  );
  // Atomic rename: tmp → dest.
  assert.match(
    source,
    /fs\.renameSync\(\s*tmp\s*,\s*dest\s*\)/,
    "install must swap runtime via fs.renameSync(tmp, dest)",
  );
  // Darwin ad-hoc codesign helper wired into staging path (before rename).
  assert.match(
    source,
    /prepareDarwinExecutable\s*\(\s*tmpBinary\s*\)/,
    "install must call prepareDarwinExecutable(tmpBinary) before runtime swap",
  );
  // Helper itself: gated on darwin, uses /usr/bin/codesign --force --sign -.
  assert.match(
    source,
    /process\.platform\s*!==\s*["']darwin["']/,
    "prepareDarwinExecutable must early-return when not on Darwin",
  );
  assert.match(
    source,
    /\/usr\/bin\/codesign["'][^)]*"--force"[^)]*"--sign"[^)]*"-"/,
    "Darwin signer must invoke /usr/bin/codesign --force --sign - (ad-hoc)",
  );
});

function tempRoot(label) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `team-agent-install-${label}-`));
}

function writeVersionScript(file, version) {
  fs.writeFileSync(
    file,
    `#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "team-agent ${version}"
  exit 0
fi
exit 2
`,
    { mode: 0o755 },
  );
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
