#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(__dirname, "..");
const packageJson = JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8"));

const command = process.argv[2] || "install";
const args = process.argv.slice(3);

if (["-h", "--help", "help"].includes(command)) {
  printHelp();
  process.exit(0);
}

if (command === "install" || command === "update") {
  install(args);
} else if (command === "doctor") {
  runDoctor(args);
} else if (command === "uninstall") {
  uninstall(args);
} else {
  console.error(`unknown command: ${command}`);
  printHelp();
  process.exit(2);
}

function printHelp() {
  console.log(`Team Agent installer

Usage:
  npx @team-agent/installer@latest install
  npx @team-agent/installer@latest doctor
  npx @team-agent/installer@latest uninstall

Options:
  --prefix <dir>       wrapper install prefix, default ~/.local
  --runtime-dir <dir>  stable runtime root, default ~/.team-agent/runtime
  --python <path>      Python executable, otherwise TEAM_AGENT_PYTHON, python3, python
  --purge-runtime      uninstall also removes the runtime root
`);
}

function install(argv) {
  const opts = parseOptions(argv);
  const python = resolvePython(opts.python);
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  const binDir = path.join(prefix, "bin");
  const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
  const version = packageJson.version || "dev";
  const dest = path.join(runtimeRoot, version);
  const tmp = path.join(runtimeRoot, `.${version}.${process.pid}.tmp`);
  const backup = path.join(runtimeRoot, `.${version}.previous`);

  fs.mkdirSync(runtimeRoot, { recursive: true });
  fs.rmSync(tmp, { recursive: true, force: true });
  copyTree(packageRoot, tmp);
  fs.rmSync(backup, { recursive: true, force: true });
  if (fs.existsSync(dest)) {
    fs.renameSync(dest, backup);
  }
  fs.renameSync(tmp, dest);

  fs.mkdirSync(binDir, { recursive: true });
  writeWrapper(path.join(binDir, "team-agent"), dest, "team_agent", python);
  writeWrapper(path.join(binDir, "team_orchestrator"), dest, "team_agent.mcp_server", python);
  writeWrapper(path.join(binDir, "team-agent-coordinator"), dest, "team_agent.coordinator", python);

  const teamAgent = path.join(binDir, "team-agent");
  const skill = spawnSync(teamAgent, ["install-skill", "--target", "all"], {
    text: true,
    encoding: "utf8",
  });
  if (skill.status !== 0) {
    process.stderr.write(skill.stderr || skill.stdout || "team-agent install-skill failed\n");
    process.exit(skill.status || 1);
  }

  console.log(`installed: ${teamAgent}`);
  console.log(`runtime: ${dest}`);
  console.log(`python: ${python}`);
  console.log(`skill: installed for Codex and Claude`);
  console.log(`PATH: ensure ${binDir} is on PATH`);

  const doctor = spawnSync(teamAgent, ["doctor", "--json"], { text: true, encoding: "utf8" });
  if (doctor.status === 0) {
    console.log("doctor: ok");
  } else {
    console.log("doctor: has blockers; run `team-agent doctor` after updating PATH");
  }
}

function runDoctor(argv) {
  const opts = parseOptions(argv);
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  const teamAgent = path.join(prefix, "bin", "team-agent");
  if (!fs.existsSync(teamAgent)) {
    console.error(`team-agent wrapper not found: ${teamAgent}`);
    process.exit(1);
  }
  const proc = spawnSync(teamAgent, ["doctor"], { stdio: "inherit" });
  process.exit(proc.status || 0);
}

function uninstall(argv) {
  const opts = parseOptions(argv);
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  for (const name of ["team-agent", "team_orchestrator", "team-agent-coordinator"]) {
    fs.rmSync(path.join(prefix, "bin", name), { force: true });
  }
  for (const skillDir of [
    path.join(os.homedir(), ".codex", "skills", "team-agent"),
    path.join(os.homedir(), ".claude", "skills", "team-agent"),
  ]) {
    fs.rmSync(skillDir, { recursive: true, force: true });
  }
  console.log(`removed wrappers from ${path.join(prefix, "bin")}`);
  console.log("removed skills from ~/.codex/skills/team-agent and ~/.claude/skills/team-agent");
  if (opts.purgeRuntime) {
    const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
    fs.rmSync(runtimeRoot, { recursive: true, force: true });
    console.log(`removed runtime root ${runtimeRoot}`);
  } else {
    console.log("runtime directories are left under ~/.team-agent/runtime for rollback; pass --purge-runtime only when no teams are running.");
  }
}

function parseOptions(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i += 1) {
    const item = argv[i];
    if (item === "--prefix") {
      opts.prefix = argv[++i];
    } else if (item.startsWith("--prefix=")) {
      opts.prefix = item.slice("--prefix=".length);
    } else if (item === "--runtime-dir") {
      opts.runtimeDir = argv[++i];
    } else if (item.startsWith("--runtime-dir=")) {
      opts.runtimeDir = item.slice("--runtime-dir=".length);
    } else if (item === "--python") {
      opts.python = argv[++i];
    } else if (item.startsWith("--python=")) {
      opts.python = item.slice("--python=".length);
    } else if (item === "--purge-runtime") {
      opts.purgeRuntime = true;
    } else {
      throw new Error(`unknown option: ${item}`);
    }
  }
  return opts;
}

function resolvePython(explicit) {
  const candidates = pythonCandidates(explicit);
  for (const candidate of candidates) {
    const resolved = path.isAbsolute(candidate) ? candidate : which(candidate) || candidate;
    if (!resolved) {
      continue;
    }
    const proc = spawnSync(resolved, ["-c", "import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)"], {
      text: true,
      encoding: "utf8",
    });
    if (proc.status === 0) {
      return resolved;
    }
  }
  console.error("No usable Python >= 3.10 found. Set TEAM_AGENT_PYTHON or pass --python.");
  process.exit(1);
}

function pythonCandidates(explicit) {
  const candidates = [explicit, process.env.TEAM_AGENT_PYTHON, "python3", "python"];
  const commonPaths = [
    "/opt/homebrew/bin/python3",
    "/usr/local/bin/python3",
    "/usr/bin/python3",
    "/opt/homebrew/opt/python@3/bin/python3",
    "/usr/local/opt/python@3/bin/python3",
  ];
  candidates.push(...commonPaths);
  for (const root of ["/opt/homebrew/opt", "/usr/local/opt"]) {
    try {
      for (const entry of fs.readdirSync(root)) {
        if (!entry.startsWith("python@")) {
          continue;
        }
        const bin = path.join(root, entry, "bin");
        for (const name of fs.readdirSync(bin)) {
          if (/^python3(\.\d+)?$/.test(name)) {
            candidates.push(path.join(bin, name));
          }
        }
      }
    } catch {
      continue;
    }
  }
  return [...new Set(candidates.filter(Boolean))];
}

function which(commandName) {
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    if (!directory) {
      continue;
    }
    const candidate = path.join(directory, commandName);
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      continue;
    }
  }
  return null;
}

function writeWrapper(file, runtimeDir, moduleName, python) {
  const content = `#!/usr/bin/env sh
PYTHON_BIN="\${TEAM_AGENT_PYTHON:-${doubleQuoteValue(python)}}"
PYTHONPATH="${doubleQuoteValue(path.join(runtimeDir, "src"))}" exec "$PYTHON_BIN" -m ${moduleName} "$@"
`;
  fs.writeFileSync(file, content, { mode: 0o755 });
  fs.chmodSync(file, 0o755);
}

function copyTree(src, dest) {
  const ignored = new Set([".git", ".team", "node_modules", "__pycache__", ".pytest_cache", ".venv"]);
  const stat = fs.lstatSync(src);
  if (stat.isDirectory()) {
    const name = path.basename(src);
    if (ignored.has(name) || src.endsWith(path.join("team-agent-core", "target"))) {
      return;
    }
    fs.mkdirSync(dest, { recursive: true, mode: stat.mode });
    for (const entry of fs.readdirSync(src)) {
      if (entry === ".DS_Store") {
        continue;
      }
      copyTree(path.join(src, entry), path.join(dest, entry));
    }
    return;
  }
  if (stat.isFile()) {
    fs.copyFileSync(src, dest);
    fs.chmodSync(dest, stat.mode);
  }
}

function expandHome(value) {
  if (value === "~") {
    return os.homedir();
  }
  if (value.startsWith("~/")) {
    return path.join(os.homedir(), value.slice(2));
  }
  return value;
}

function doubleQuoteValue(value) {
  return String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\$/g, "\\$").replace(/`/g, "\\`");
}
