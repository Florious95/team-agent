#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const modulePath = fileURLToPath(import.meta.url);
const __dirname = path.dirname(modulePath);
const packageRoot = path.resolve(__dirname, "..");
const require = createRequire(import.meta.url);
const packageJson = JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8"));
const DOCTOR_TIMEOUT_MS = 5000;
const VERSION_SMOKE_TIMEOUT_MS = 5000;

if (isCliEntrypoint()) {
  main();
}

function main() {
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
}

function isCliEntrypoint() {
  if (!process.argv[1]) {
    return false;
  }
  try {
    return fs.realpathSync(process.argv[1]) === fs.realpathSync(modulePath);
  } catch {
    return path.resolve(process.argv[1]) === modulePath;
  }
}

export function doctorSelfCheckVerdict(doctorBody, spawnMeta = {}) {
  if (spawnMeta.error || spawnMeta.signal) {
    return { kind: "advisory", blockers: [] };
  }

  let body;
  try {
    body = JSON.parse(doctorBody || "");
  } catch {
    return { kind: "advisory", blockers: [] };
  }

  const blockers = [];
  // Doctor JSON source: crates/team-agent/src/cli/mod.rs:2739-2764.
  if (body?.tmux?.installed === false) {
    blockers.push("tmux not installed");
  }
  if (!body?.mcp?.server_command) {
    blockers.push("MCP server command missing");
  }
  const profileSmokeStatus = body?.profile_smoke?.status;
  const profileSmokeNonBlocking =
    profileSmokeStatus === "legacy_team_invalid" || profileSmokeStatus === "not_required";
  if (
    body?.error === "profile_smoke_failed" ||
    (body?.profile_smoke?.ok === false && !profileSmokeNonBlocking)
  ) {
    blockers.push("profile smoke failed");
  }

  if (blockers.length > 0) {
    return { kind: "blockers", blockers };
  }

  const noContext =
    body?.ok === false && body?.error === "workspace has no Team Agent spec or runtime context";
  if (body?.ok === true || noContext) {
    return { kind: "ok", blockers: [] };
  }

  return { kind: "advisory", blockers: [] };
}

export function doctorSelfCheckLine(verdict) {
  if (verdict.kind === "blockers") {
    return `doctor: found blockers (${verdict.blockers.join("; ")}); run team-agent doctor in your project for details`;
  }
  return "doctor: ok (run team-agent doctor inside a team workspace for a full report)";
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
  installSkills(runtimeBinary);

  const teamAgent = path.join(binDir, "team-agent");
  console.log(`installed: ${teamAgent}`);
  console.log(`runtime: ${dest}`);
  console.log(`binary: ${platformBinary.packageName}`);
  console.log("skill: installed for Codex, Claude and Copilot");
  console.log(`PATH: ensure ${binDir} is on PATH`);

  // 0.3.6 hotfix · C-5 cr verdict — post-install binary smoke 门(走 `--help`
  // 子命令,因为 0.3.x CLI 现阶段没有 --version)。真跑一次 binary 才能抓住
  // loader 级失败(glibc 不兼容 / cpu 错配 / 下载损坏 / 平台子包未装到位 等),
  // 不止依赖 file 元数据。失败输出走三行式(错/动作/日志),非零退出。
  // C-2 cr verdict 守护:本步不做 libc 探测、不读 /lib/x86_64-linux-gnu/libc.so.6,
  // 通用 smoke 而非 platform-aware 逻辑。
  const binarySmoke = spawnSync(teamAgent, ["--help"], {
    text: true,
    encoding: "utf8",
    timeout: VERSION_SMOKE_TIMEOUT_MS,
  });
  if (binarySmoke.status !== 0) {
    const log = (binarySmoke.stderr || binarySmoke.stdout || "").trim() || "no stderr/stdout";
    console.error(`ERROR: team-agent --help failed (status=${binarySmoke.status ?? "signal"})`);
    console.error(`ACTION: verify your platform is supported, reinstall, or open an issue with the log below`);
    console.error(`LOG: ${teamAgent} --help => ${log}`);
    process.exit(1);
  }
  console.log("smoke: team-agent --help ok");

  const doctorWorkspace = makeDoctorWorkspace();
  try {
    const doctor = spawnSync(teamAgent, ["doctor", "--json", "--workspace", doctorWorkspace], {
      text: true,
      encoding: "utf8",
      timeout: DOCTOR_TIMEOUT_MS,
    });
    const verdict = doctorSelfCheckVerdict(doctor.stdout, doctor);
    console.log(doctorSelfCheckLine(verdict));
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
  // 卸载 skill 走二进制单源(同一 SkillTarget 表 codex/claude/copilot),在删 wrapper 前调
  // (删 wrapper 后 PATH 上的 team-agent 没了,但 runtime 二进制仍在;用 runtime 二进制直调)。
  const teamAgentBin = path.join(prefix, "bin", "team-agent");
  if (fs.existsSync(teamAgentBin)) {
    const res = spawnSync(teamAgentBin, ["install-skill", "--target", "all", "--uninstall", "--json"], {
      text: true,
      encoding: "utf8",
      timeout: VERSION_SMOKE_TIMEOUT_MS,
    });
    if (res.status !== 0) {
      console.error(`WARN: skill uninstall via binary failed (status=${res.status ?? "signal"}); skill dirs may remain under ~/.codex|.claude|.copilot/skills/team-agent`);
    }
  }
  for (const name of ["team-agent", "team_orchestrator", "team-agent-coordinator"]) {
    fs.rmSync(path.join(prefix, "bin", name), { force: true });
  }
  console.log(`removed wrappers from ${path.join(prefix, "bin")}`);
  console.log("removed skills from ~/.codex, ~/.claude and ~/.copilot skills/team-agent");
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

// RED-1 根治(单源):skill 安装唯一实现在二进制 `install-skill`(SkillTarget 表:
// codex/claude/copilot)。install.mjs 不再有自己的 JS 拷贝逻辑/目标硬编码——改调二进制,
// 失败显式报错(非零退出),绝不静默回退 JS。
function installSkills(runtimeBinary) {
  const source = path.join(packageRoot, "skills", "team-agent");
  if (!fs.existsSync(source)) {
    throw new Error(`skill source not found: ${source}`);
  }
  const res = spawnSync(runtimeBinary, ["install-skill", "--target", "all", "--source", source, "--json"], {
    text: true,
    encoding: "utf8",
    timeout: VERSION_SMOKE_TIMEOUT_MS,
  });
  if (res.status !== 0) {
    const log = (res.stderr || res.stdout || "").trim() || "no stderr/stdout";
    console.error(`ERROR: skill install failed (status=${res.status ?? "signal"})`);
    console.error(`ACTION: reinstall, or run \`team-agent install-skill --target all --source ${source}\` manually`);
    console.error(`LOG: ${runtimeBinary} install-skill --target all => ${log}`);
    process.exit(1);
  }
}

function makeDoctorWorkspace() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "team-agent-doctor-"));
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
