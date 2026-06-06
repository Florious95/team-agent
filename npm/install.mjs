#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(__dirname, "..");
const require = createRequire(import.meta.url);
const packageJson = JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8"));
const DOCTOR_TIMEOUT_MS = 5000;

const command = process.argv[2] || "install";
const args = process.argv.slice(3);

if (["-h", "--help", "help"].includes(command)) {
  printHelp();
  process.exit(0);
}

try {
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
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
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
  --purge-runtime      uninstall also removes the runtime root
`);
}

function install(argv) {
  const opts = parseOptions(argv);
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  const binDir = path.join(prefix, "bin");
  const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
  const version = packageJson.version || "dev";
  const dest = path.join(runtimeRoot, version);
  const tmp = path.join(runtimeRoot, `.${version}.${process.pid}.tmp`);
  const backup = path.join(runtimeRoot, `.${version}.previous`);
  const platformBinary = resolvePlatformBinary();

  fs.mkdirSync(runtimeRoot, { recursive: true });
  fs.rmSync(tmp, { recursive: true, force: true });
  fs.mkdirSync(path.join(tmp, "bin"), { recursive: true });
  copyExecutable(platformBinary.path, path.join(tmp, "bin", "team-agent"));

  fs.rmSync(backup, { recursive: true, force: true });
  if (fs.existsSync(dest)) {
    fs.renameSync(dest, backup);
  }
  fs.renameSync(tmp, dest);

  const runtimeBinary = path.join(dest, "bin", "team-agent");
  fs.mkdirSync(binDir, { recursive: true });
  writeExecWrapper(path.join(binDir, "team-agent"), runtimeBinary, []);
  writeExecWrapper(path.join(binDir, "team_orchestrator"), runtimeBinary, ["mcp-server"]);
  writeExecWrapper(path.join(binDir, "team-agent-coordinator"), runtimeBinary, ["coordinator"]);
  installSkills();

  const teamAgent = path.join(binDir, "team-agent");
  console.log(`installed: ${teamAgent}`);
  console.log(`runtime: ${dest}`);
  console.log(`binary: ${platformBinary.packageName}`);
  console.log("skill: installed for Codex and Claude");
  console.log(`PATH: ensure ${binDir} is on PATH`);

  const doctorWorkspace = makeDoctorWorkspace();
  try {
    const doctor = spawnSync(teamAgent, ["doctor", "--json", "--workspace", doctorWorkspace], {
      text: true,
      encoding: "utf8",
      timeout: DOCTOR_TIMEOUT_MS,
    });
    if (doctor.status === 0) {
      console.log("doctor: ok");
    } else {
      console.log("doctor: has blockers; run `team-agent doctor` after updating PATH");
    }
  } finally {
    fs.rmSync(doctorWorkspace, { recursive: true, force: true });
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
  const doctorWorkspace = makeDoctorWorkspace();
  try {
    const proc = spawnSync(teamAgent, ["doctor", "--workspace", doctorWorkspace], {
      stdio: "inherit",
      timeout: DOCTOR_TIMEOUT_MS,
    });
    process.exit(proc.status ?? 1);
  } finally {
    fs.rmSync(doctorWorkspace, { recursive: true, force: true });
  }
}

function uninstall(argv) {
  const opts = parseOptions(argv);
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  for (const name of ["team-agent", "team_orchestrator", "team-agent-coordinator"]) {
    fs.rmSync(path.join(prefix, "bin", name), { force: true });
  }
  for (const skillDir of skillDestinations()) {
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
    } else if (item?.startsWith("--prefix=")) {
      opts.prefix = item.slice("--prefix=".length);
    } else if (item === "--runtime-dir") {
      opts.runtimeDir = argv[++i];
    } else if (item?.startsWith("--runtime-dir=")) {
      opts.runtimeDir = item.slice("--runtime-dir=".length);
    } else if (item === "--purge-runtime") {
      opts.purgeRuntime = true;
    } else {
      throw new Error(`unknown option: ${item}`);
    }
  }
  return opts;
}

function resolvePlatformBinary() {
  const packageName = platformPackageName();
  if (!packageName) {
    printUnsupportedPlatform();
    process.exit(1);
  }
  const binarySpec = `${packageName}/bin/team-agent`;
  try {
    return {
      packageName,
      path: require.resolve(binarySpec),
    };
  } catch {
    console.error(`ERROR: Team Agent binary package not installed for ${process.platform}/${process.arch}`);
    console.error("ACTION: rerun without --no-optional, or use a supported macOS/Linux/WSL platform.");
    console.error(`LOG: package=${packageName} node=${process.version} platform=${process.platform} arch=${process.arch}`);
    process.exit(1);
  }
}

function platformPackageName() {
  const key = `${process.platform}-${process.arch}`;
  const packages = {
    "darwin-arm64": "@team-agent/cli-darwin-arm64",
    "darwin-x64": "@team-agent/cli-darwin-x64",
    "linux-x64": "@team-agent/cli-linux-x64",
  };
  return packages[key] || null;
}

function printUnsupportedPlatform() {
  console.error(`ERROR: unsupported Team Agent platform ${process.platform}/${process.arch}.`);
  console.error("ACTION: supported platforms are darwin/arm64, darwin/x64, and linux/x64.");
  console.error(`LOG: node=${process.version} platform=${process.platform} arch=${process.arch}`);
}

function copyExecutable(src, dest) {
  fs.copyFileSync(src, dest);
  fs.chmodSync(dest, 0o755);
}

function writeExecWrapper(file, binary, fixedArgs) {
  const args = fixedArgs.map(shellQuote).join(" ");
  const argPrefix = args ? `${args} ` : "";
  const content = `#!/usr/bin/env sh
exec ${shellQuote(binary)} ${argPrefix}"$@"
`;
  fs.writeFileSync(file, content, { mode: 0o755 });
  fs.chmodSync(file, 0o755);
}

function installSkills() {
  const source = path.join(packageRoot, "skills", "team-agent");
  if (!fs.existsSync(source)) {
    throw new Error(`skill source not found: ${source}`);
  }
  for (const dest of skillDestinations()) {
    fs.rmSync(dest, { recursive: true, force: true });
    copyTree(source, dest);
  }
}

function skillDestinations() {
  return [
    path.join(os.homedir(), ".codex", "skills", "team-agent"),
    path.join(os.homedir(), ".claude", "skills", "team-agent"),
  ];
}

function makeDoctorWorkspace() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "team-agent-doctor-"));
}

function copyTree(src, dest) {
  const stat = fs.lstatSync(src);
  if (stat.isDirectory()) {
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

function shellQuote(value) {
  return `'${String(value).replace(/'/g, "'\\''")}'`;
}
