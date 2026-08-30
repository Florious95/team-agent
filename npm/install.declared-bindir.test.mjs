/**
 * purpose: named regression for declared-only wrapper landing and old-wrapper reclaim
 * contract: third-party writable PATH dirs are never chosen; declared dir unwritable fails closed with ACTION; manifest-recorded installer wrappers are rewritten onto the new runtime
 * boundary: uses disposable HOME trees only; does not touch the host login PATH or real $HOME
 */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  recordedManagedBinDirs,
  repairPathShadowingTeamAgentCommands,
  resolveInstallBinDir,
} from "./install.mjs";

test("declared bindir ignores a writable unrelated PATH dir in front", () => {
  const root = tempRoot("third-party-first");
  const unrelated = path.join(root, "opt", "openjdk", "bin");
  const declared = path.join(root, ".local", "bin");
  fs.mkdirSync(unrelated, { recursive: true });
  fs.mkdirSync(declared, { recursive: true });

  const resolved = resolveInstallBinDir({
    env: { PATH: [unrelated, declared, "/usr/bin", "/bin"].join(path.delimiter), SHELL: "/bin/zsh" },
    home: root,
  });

  assert.equal(resolved.binDir, declared);
  assert.equal(resolved.kind, "declared");
  assert.notEqual(resolved.binDir, unrelated);
  assert.equal(fs.existsSync(path.join(unrelated, "team-agent")), false);
});

test("declared bindir fails closed when the product landing is not writable", () => {
  const root = tempRoot("declared-ro");
  const declared = path.join(root, ".local", "bin");
  fs.mkdirSync(declared, { recursive: true });
  fs.chmodSync(declared, 0o555);

  let error;
  try {
    resolveInstallBinDir({
      env: { PATH: [path.join(root, "opt", "openjdk", "bin"), declared].join(path.delimiter), SHELL: "/bin/zsh" },
      home: root,
    });
  } catch (caught) {
    error = caught;
  } finally {
    fs.chmodSync(declared, 0o755);
  }

  assert.ok(error instanceof Error, "must throw instead of picking another PATH dir");
  assert.match(error.message, /ERROR: cannot write Team Agent wrapper directory/);
  assert.match(error.message, /ACTION: make it writable/);
  assert.match(error.message, /--prefix/);
  assert.match(error.message, /does not fall back to a PATH entry/);
});

test("reclaim rewrites a manifest-recorded old wrapper onto the new runtime", () => {
  const root = tempRoot("reclaim-manifest");
  const declared = path.join(root, ".local", "bin");
  const oldBin = path.join(root, "opt", "openjdk", "bin");
  const oldRuntime = path.join(root, "runtime", "old", "bin", "team-agent");
  const newRuntime = path.join(root, "runtime", "new", "bin", "team-agent");
  fs.mkdirSync(declared, { recursive: true });
  fs.mkdirSync(oldBin, { recursive: true });
  fs.mkdirSync(path.dirname(newRuntime), { recursive: true });
  fs.writeFileSync(newRuntime, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  writeManagedWrapper(path.join(oldBin, "team-agent"), oldRuntime);
  writeManagedWrapper(path.join(declared, "team-agent"), newRuntime);

  const manifest = {
    binDir: oldBin,
    pathShadowRepairs: [path.join(oldBin, "team-agent")],
  };
  assert.deepEqual(recordedManagedBinDirs(manifest), [oldBin]);

  const repairs = repairPathShadowingTeamAgentCommands({
    env: { PATH: [oldBin, declared].join(path.delimiter) },
    home: root,
    binDir: declared,
    runtimeBinary: newRuntime,
    recordedBinDirs: recordedManagedBinDirs(manifest),
  });

  assert.ok(repairs.some((repair) => repair.file === path.join(oldBin, "team-agent")));
  const oldText = fs.readFileSync(path.join(oldBin, "team-agent"), "utf8");
  const newText = fs.readFileSync(path.join(declared, "team-agent"), "utf8");
  assert.match(oldText, new RegExp(escapeRegExp(newRuntime)));
  assert.match(newText, new RegExp(escapeRegExp(newRuntime)));
});

function writeManagedWrapper(file, runtimeBinary) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    `#!/usr/bin/env sh
# team-agent installer wrapper
exec '${runtimeBinary}' "$@"
`,
    { mode: 0o755 },
  );
}

function tempRoot(label) {
  return fs.mkdtempSync(path.join(os.tmpdir(), `team-agent-declared-bindir-${label}-`));
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
