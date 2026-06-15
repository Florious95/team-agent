import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  doctorSelfCheckLine,
  doctorSelfCheckVerdict,
  readInstallManifest,
  resolveInstallBinDir,
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

test("install manifest persists selected bin dir for later commands", () => {
  const root = tempRoot("manifest");
  const runtimeRoot = path.join(root, "runtime");
  const binDir = path.join(root, "bin");

  writeInstallManifest(runtimeRoot, { binDir, version: "test-version" });
  const manifest = readInstallManifest(runtimeRoot);

  assert.equal(manifest.binDir, binDir);
  assert.equal(manifest.version, "test-version");
});

function tempRoot(label) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `team-agent-install-${label}-`));
}
