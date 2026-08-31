#!/usr/bin/env node
/** Resolve local Markdown links and optionally validate package/installed inventories. */
import fs from "node:fs";
import path from "node:path";

const args = process.argv.slice(2);
const value = (name) => { const i = args.indexOf(name); return i < 0 ? null : args[i + 1]; };
const rootArg = value("--root") ?? "skills/team-agent";
const root = path.resolve(rootArg);
const installedArg = value("--installed");
const inventoryArg = value("--inventory");
const markdownLink = /!?(?:\[[^\]]*\])\(([^)]+)\)/g;
const files = [];
const fail = (message) => { throw new Error(message); };
const real = (file) => { try { return fs.realpathSync.native(file); } catch (error) { fail(`cannot resolve ${file}: ${error.message}`); } };
const rootReal = real(root);
const inside = (candidate, base = rootReal) => candidate === base || candidate.startsWith(`${base}${path.sep}`);
const lexicalInside = (candidate, base = root) => candidate === base || candidate.startsWith(`${base}${path.sep}`);

function walk(dir, boundary = rootReal) {
  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name)); }
  catch (error) { fail(`cannot read ${dir}: ${error.message}`); }
  for (const entry of entries) {
    const file = path.join(dir, entry.name);
    const fileReal = real(file);
    if (!inside(fileReal, boundary)) fail(`${file}: realpath escapes tree`);
    if (entry.isDirectory()) walk(file, boundary);
    else if (entry.isFile() && file.endsWith(".md")) files.push(file);
  }
}
function normalizeFragment(fragment) {
  return decodeURIComponent(fragment).toLowerCase().trim().replace(/<[^>]+>/g, "")
    .replace(/[`*_~]/g, "").replace(/[^\p{L}\p{N} -]/gu, "").replace(/\s+/g, "-");
}
function anchors(text) {
  const found = new Set();
  const used = new Map();
  const add = (value) => {
    const base = normalizeFragment(value); if (!base) return;
    const n = used.get(base) ?? 0; used.set(base, n + 1); found.add(n ? `${base}-${n}` : base);
  };
  for (const m of text.matchAll(/^(?: {0,3})#{1,6}\s+(.+?)\s*#*\s*$/gm) ) add(m[1]);
  for (const m of text.matchAll(/^(?: {0,3})([^\n#]+)\n(?:=+|-+)\s*$/gm)) add(m[1]);
  for (const m of text.matchAll(/<(?:a|[^>]+)\s+[^>]*(?:id|name)=["']([^"']+)["'][^>]*>/gi)) add(m[1]);
  return found;
}
function checkTree(dir, baseRoot, baseReal) {
  files.length = 0; walk(dir, baseReal);
  const errors = [];
  for (const file of [...files].sort()) {
    const text = fs.readFileSync(file, "utf8");
    const known = anchors(text);
    for (const match of text.matchAll(markdownLink)) {
      const href = match[1].trim();
      if (!href || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href)) continue;
      const hash = href.indexOf("#");
      const rawPath = hash < 0 ? href : href.slice(0, hash);
      const fragment = hash < 0 ? "" : href.slice(hash + 1).split("?", 1)[0];
      const target = rawPath ? path.resolve(path.dirname(file), rawPath) : file;
      if (!lexicalInside(target, baseRoot)) { errors.push(`${file}: lexical target escapes tree: ${href}`); continue; }
      const targetReal = real(target);
      if (!inside(targetReal, baseReal)) { errors.push(`${file}: realpath target escapes tree: ${href}`); continue; }
      if (!fs.statSync(targetReal).isFile()) { errors.push(`${file}: target is not a file: ${href}`); continue; }
      if (fragment && targetReal.endsWith(".md")) {
        const targetAnchors = targetReal === real(file) ? known : anchors(fs.readFileSync(targetReal, "utf8"));
        if (!targetAnchors.has(normalizeFragment(fragment))) errors.push(`${file}: missing fragment: ${href}`);
      }
    }
  }
  if (errors.length) fail(errors.sort().join("\n"));
  return [...files].sort().length;
}
const count = checkTree(root, root, rootReal);
if (inventoryArg) {
  const inventory = path.resolve(inventoryArg);
  for (const relative of fs.readFileSync(inventory, "utf8").split(/\r?\n/).filter(Boolean).sort()) {
    if (path.isAbsolute(relative)) fail(`inventory path must be relative: ${relative}`);
    const candidate = path.resolve(root, relative);
    if (!lexicalInside(candidate)) fail(`inventory path escapes tree: ${relative}`);
    const candidateReal = real(candidate);
    if (!inside(candidateReal)) fail(`inventory realpath escapes tree: ${relative}`);
  }
}
if (installedArg) {
  const installed = path.resolve(installedArg); console.log(`checked ${checkTree(installed, installed, real(installed))} Markdown files in ${installed}`);
}
console.log(`checked ${count} Markdown files in ${root}`);
