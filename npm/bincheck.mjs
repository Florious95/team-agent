#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";

const BIN_NAME = "team-agent-installer";
const SELF_CHECK_ONLY = process.env.TEAM_AGENT_INSTALLER_SELF_CHECK_ONLY === "1";

const initCwd = process.env.INIT_CWD || process.cwd();
const pathValue = process.env.PATH || "";

if (!findOnPath(BIN_NAME, pathValue)) {
  printMissingBinDiagnostic(initCwd, pathValue);
  process.exit(SELF_CHECK_ONLY ? 1 : 0);
}

if (SELF_CHECK_ONLY) {
  process.exit(0);
}

function findOnPath(commandName, searchPath) {
  const extensions = process.platform === "win32" ? ["", ".cmd", ".bat", ".ps1", ".exe"] : [""];
  for (const directory of searchPath.split(path.delimiter)) {
    if (!directory) {
      continue;
    }
    for (const extension of extensions) {
      const candidate = path.join(directory, `${commandName}${extension}`);
      try {
        fs.accessSync(candidate, fs.constants.X_OK);
        return candidate;
      } catch {
        continue;
      }
    }
  }
  return null;
}

function printMissingBinDiagnostic(cwd, searchPath) {
  const npmrcPath = path.join(cwd, ".npmrc");
  const npmrcSummary = summarizeNpmrc(npmrcPath);
  const wslHint = isWslMntC(cwd) ? "yes" : "unknown";
  const pathEntries = searchPath ? searchPath.split(path.delimiter).length : 0;

  console.error("ERROR: team-agent-installer bin not on PATH after npm install.");
  console.error("ACTION: This is common when WSL runs npx from /mnt/c and a project-level .npmrc sets prefix.");
  console.error("        - Move that setting to ~/.npmrc, or delete the project-level prefix line.");
  console.error("        - Then cd ~ and rerun `npx @team-agent/installer@latest install`.");
  console.error("LOG:");
  console.error(`  INIT_CWD=${cwd}`);
  console.error(`  WSL_MNT_C=${wslHint}`);
  console.error(`  npmrc=${npmrcSummary}`);
  console.error(`  PATH_ENTRIES=${pathEntries}`);
}

function summarizeNpmrc(npmrcPath) {
  if (!fs.existsSync(npmrcPath)) {
    return `${npmrcPath} missing`;
  }
  const text = fs.readFileSync(npmrcPath, "utf8");
  const hasPrefix = text
    .split(/\r?\n/)
    .some((line) => /^\s*prefix\s*=/.test(line) && !/^\s*[#;]/.test(line));
  return `${npmrcPath} present prefix=${hasPrefix ? "yes" : "no"}`;
}

function isWslMntC(value) {
  const normalized = value.replace(/\\/g, "/").toLowerCase();
  return normalized.startsWith("/mnt/c/");
}
