#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const checker = path.resolve("npm/check-skill-links.mjs");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "ta-links-"));
const root = path.join(tmp, "skill");
fs.mkdirSync(root); fs.writeFileSync(path.join(root, "ok.md"), "# Valid Heading\n\n<a id=\"explicit\"></a>\n[heading](ok.md#valid-heading) [explicit](#explicit)\n");
const run = (...args) => spawnSync(process.execPath, [checker, "--root", root, ...args], { encoding: "utf8" });
assert.equal(run().status, 0, "valid heading and explicit anchor");
const expectFail = (name, setup, ...args) => { setup(); const result = run(...args); assert.notEqual(result.status, 0, name); return result.stderr; };
expectFail("missing fragment", () => fs.writeFileSync(path.join(root, "ok.md"), "[x](ok.md#missing)\n"));
expectFail("lexical escape", () => fs.writeFileSync(path.join(root, "ok.md"), "[x](../outside.md)\n"));
const outside = path.join(tmp, "outside.md"); fs.writeFileSync(outside, "# Outside\n");
expectFail("symlink escape", () => { fs.rmSync(path.join(root, "ok.md")); fs.symlinkSync(outside, path.join(root, "ok.md")); });
fs.rmSync(path.join(root, "ok.md")); fs.writeFileSync(path.join(root, "ok.md"), "# Order\n");
const inventory = path.join(tmp, "inventory.txt"); fs.writeFileSync(inventory, "../outside.md\n");
expectFail("inventory lexical escape", () => {}, "--inventory", inventory);
fs.writeFileSync(inventory, `${path.join(tmp, "outside.md")}\n`);
expectFail("inventory absolute escape", () => {}, "--inventory", inventory);
fs.writeFileSync(path.join(root, "b.md"), "[x](missing-b.md)\n"); fs.writeFileSync(path.join(root, "a.md"), "[x](missing-a.md)\n");
const first = run().stderr, second = run().stderr; assert.equal(first, second, "deterministic error ordering");
console.log("link checker fixtures: valid fragment, missing fragment, lexical/symlink/absolute escapes, deterministic ordering: ok");
