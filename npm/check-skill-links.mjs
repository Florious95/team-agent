#!/usr/bin/env node
/** Resolve local Markdown links and optionally validate package/installed inventories. */
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const value = (name) => { const i = args.indexOf(name); return i < 0 ? null : args[i + 1]; };
const root = path.resolve(value("--root") ?? "skills/team-agent");
const installed = value("--installed") && path.resolve(value("--installed"));
const inventory = value("--inventory") && path.resolve(value("--inventory"));
const markdownLink = /!?(?:\[[^\]]*\])\(([^)]+)\)/g;
const files = [];
function walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(file);
    else if (entry.isFile() && file.endsWith(".md")) files.push(file);
  }
}
function checkTree(dir) {
  const errors = [];
  files.length = 0;
  walk(dir);
  for (const file of files) {
    const text = fs.readFileSync(file, "utf8");
    for (const match of text.matchAll(markdownLink)) {
      const target = match[1].trim().split(/[?#]/, 1)[0];
      if (!target || /^(?:[a-z]+:|\/\/)/i.test(target)) continue;
      const resolved = path.resolve(path.dirname(file), target);
      if (resolved !== dir && !resolved.startsWith(`${dir}${path.sep}`)) errors.push(`${file}: escapes tree: ${target}`);
      else if (!fs.existsSync(resolved)) errors.push(`${file}: missing target: ${target}`);
    }
  }
  if (errors.length) throw new Error(errors.join("\n"));
  return files.length;
}
const count = checkTree(root);
if (inventory) {
  for (const relative of fs.readFileSync(inventory, "utf8").split(/\r?\n/).filter(Boolean)) {
    if (!fs.existsSync(path.resolve(root, relative))) throw new Error(`inventory missing: ${relative}`);
  }
}
if (installed) console.log(`checked ${checkTree(installed)} Markdown files in ${installed}`);
console.log(`checked ${count} Markdown files in ${root}`);
