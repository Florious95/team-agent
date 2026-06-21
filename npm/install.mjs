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
const INSTALL_MANIFEST = "install-manifest.json";
const PATH_MARKER = "# team-agent PATH (E48)";
const WRAPPER_MARKER = "# team-agent installer wrapper";
const WRAPPER_NAMES = ["team-agent", "team_orchestrator", "team-agent-coordinator"];

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
  --prefix <dir>       fallback wrapper prefix when no writable PATH dir exists, default ~/.local
  --runtime-dir <dir>  stable runtime root, default ~/.team-agent/runtime
  --purge-runtime      uninstall also removes the runtime root
`);
}

function install(argv) {
  const opts = parseOptions(argv);
  const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
  const installTarget = resolveInstallBinDir({ env: process.env, home: os.homedir(), prefix: opts.prefix });
  const binDir = installTarget.binDir;
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
  writeExecWrapper(path.join(binDir, "team-agent"), runtimeBinary, [], { allowForeign: true });
  writeExecWrapper(path.join(binDir, "team_orchestrator"), runtimeBinary, ["mcp-server"]);
  writeExecWrapper(path.join(binDir, "team-agent-coordinator"), runtimeBinary, ["coordinator"]);
  const shadowRepairs = repairPathShadowingTeamAgentCommands({
    env: process.env,
    home: os.homedir(),
    binDir,
    runtimeBinary,
    log: (line) => console.log(line),
  });
  installSkills(runtimeBinary);
  writeInstallManifest(runtimeRoot, {
    version,
    binDir,
    runtimeRoot,
    runtimeBinary,
    installedAt: new Date().toISOString(),
    installTargetKind: installTarget.kind,
    pathShadowRepairs: shadowRepairs.map((repair) => repair.file),
  });

  const teamAgent = path.join(binDir, "team-agent");
  console.log(`installed: ${teamAgent}`);
  if (installTarget.readyNow) {
    console.log(`installed to ${binDir} (on PATH, ready now)`);
  } else if (installTarget.rc?.files?.length > 0) {
    console.log(`installed to ${binDir}; added ${binDir} to ${installTarget.rc.files.join(", ")}; restart terminal or open a new shell to use team-agent`);
  } else if (installTarget.rc?.skipped?.length > 0) {
    console.log(`installed to ${binDir}; PATH entry already present in ${installTarget.rc.skipped.join(", ")}; restart terminal or open a new shell to use team-agent`);
  } else {
    console.log(`installed to ${binDir}; add it to PATH to use team-agent by name`);
  }
  console.log(`runtime: ${dest}`);
  console.log(`binary: ${platformBinary.packageName}`);
  console.log("skill: installed for Codex, Claude and Copilot");

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
  const pathCheck = verifyInstalledTeamAgentOnPath({
    env: process.env,
    expectedVersion: version,
  });
  console.log(`post-install: ${pathCheck.entry} --version = ${pathCheck.version}`);
}

function runDoctor(argv) {
  const opts = parseOptions(argv);
  const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
  const teamAgent = path.join(installedBinDir(runtimeRoot, opts), "team-agent");
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
  const runtimeRoot = path.resolve(expandHome(opts.runtimeDir || path.join(os.homedir(), ".team-agent", "runtime")));
  const binDir = installedBinDir(runtimeRoot, opts);
  // 卸载 skill 走二进制单源(同一 SkillTarget 表 codex/claude/copilot),在删 wrapper 前调
  // (删 wrapper 后 PATH 上的 team-agent 没了,但 runtime 二进制仍在;用 runtime 二进制直调)。
  const teamAgentBin = path.join(binDir, "team-agent");
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
  for (const name of WRAPPER_NAMES) {
    fs.rmSync(path.join(binDir, name), { force: true });
  }
  console.log(`removed wrappers from ${binDir}`);
  console.log("removed skills from ~/.codex, ~/.claude and ~/.copilot skills/team-agent");
  if (opts.purgeRuntime) {
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

export function resolveInstallBinDir(options = {}) {
  const env = options.env || process.env;
  const home = options.home || os.homedir();
  const entries = uniquePathEntries(env.PATH || "", home);
  for (const entry of entries) {
    if (isVersionManagedPath(entry) || !canWriteDir(entry) || hasForeignWrapper(entry)) {
      continue;
    }
    return { binDir: entry, kind: "path", readyNow: true, rc: null };
  }

  for (const entry of entries) {
    if (isVersionManagedPath(entry) || !isReasonableUserBinDir(entry, home) || !canWriteDir(entry) || hasForeignWrapper(entry)) {
      continue;
    }
    return { binDir: entry, kind: "path_user", readyNow: true, rc: null };
  }

  const fallbackPrefix = options.prefix
    ? path.resolve(expandHomeFor(options.prefix, home))
    : path.join(home, ".local");
  const binDir = path.join(fallbackPrefix, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  const rc = ensureBinDirOnShellRc(binDir, { env, home });
  return { binDir, kind: "shell_rc", readyNow: false, rc };
}

export function repairPathShadowingTeamAgentCommands(options = {}) {
  const env = options.env || process.env;
  const home = options.home || os.homedir();
  const binDir = path.resolve(expandHomeFor(options.binDir || "", home));
  const runtimeBinary = options.runtimeBinary;
  const log = typeof options.log === "function" ? options.log : null;
  if (!runtimeBinary) {
    throw new Error("runtimeBinary is required");
  }
  const installedWrapper = path.join(binDir, "team-agent");
  const repairs = [];
  const candidates = pathShadowRepairCandidates(env.PATH || "", home, binDir);
  log?.(`path-shadow: scanning ${candidates.length} candidate bin dirs before ${binDir}`);
  for (const candidateDir of candidates) {
    const entry = candidateDir.dir;
    const candidate = path.join(entry, "team-agent");
    if (isVersionManagedPath(entry)) {
      log?.(`path-shadow: skip ${candidate} source=${candidateDir.source} reason=version-managed-path`);
      continue;
    }
    if (!fs.existsSync(candidate)) {
      if (candidateDir.source === "home-local-bin") {
        log?.(`path-shadow: checked ${candidate} source=${candidateDir.source} reason=not-found`);
      }
      continue;
    }
    log?.(`path-shadow: found ${candidate} source=${candidateDir.source}`);
    if (!isExecutableFile(candidate)) {
      log?.(`path-shadow: skip ${candidate} source=${candidateDir.source} reason=not-executable-file`);
      continue;
    }
    if (sameFile(candidate, installedWrapper)) {
      log?.(`path-shadow: skip ${candidate} source=${candidateDir.source} reason=installed-wrapper`);
      continue;
    }
    if (sameFile(candidate, runtimeBinary)) {
      log?.(`path-shadow: skip ${candidate} source=${candidateDir.source} reason=runtime-binary`);
      continue;
    }
    try {
      writeExecWrapper(candidate, runtimeBinary, [], { allowForeign: true });
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw new Error(`failed to update PATH-shadowing team-agent at ${candidate}: ${detail}`);
    }
    log?.(`path-shadow: updated ${candidate} source=${candidateDir.source} to runtime shim`);
    repairs.push({ file: candidate, binDir: entry, source: candidateDir.source });
  }
  if (repairs.length === 0) {
    log?.("path-shadow: no stale team-agent command repaired");
  }
  return repairs;
}

export function verifyInstalledTeamAgentOnPath(options = {}) {
  const env = options.env || process.env;
  const expectedVersion = options.expectedVersion;
  if (!expectedVersion) {
    throw new Error("expectedVersion is required");
  }
  const which = spawnSync("which", ["team-agent"], {
    text: true,
    encoding: "utf8",
    env,
    timeout: VERSION_SMOKE_TIMEOUT_MS,
  });
  const entry = (which.stdout || "").split(/\r?\n/).map((line) => line.trim()).find(Boolean);
  if (which.status !== 0 || !entry) {
    throw new Error(
      `post-install version check failed: \`which team-agent\` did not find the installed command. ` +
        `Expected version ${expectedVersion}. Add the install bin directory to PATH or restart your shell.`,
    );
  }
  const versionProbe = spawnSync(entry, ["--version"], {
    text: true,
    encoding: "utf8",
    env,
    timeout: VERSION_SMOKE_TIMEOUT_MS,
  });
  const versionOutput = `${versionProbe.stdout || ""}\n${versionProbe.stderr || ""}`;
  if (versionProbe.status !== 0) {
    const log = versionOutput.trim() || "no stderr/stdout";
    throw new Error(
      `post-install version check failed: PATH resolves team-agent to ${entry}, but \`${entry} --version\` failed ` +
        `(status=${versionProbe.status ?? "signal"}). Expected version ${expectedVersion}. ` +
        `A stale binary may be shadowing the installed shim. LOG: ${log}`,
    );
  }
  const actualVersion = parseTeamAgentVersion(versionOutput);
  if (actualVersion !== expectedVersion) {
    throw new Error(
      `post-install version check failed: PATH resolves team-agent to ${entry}, version ${actualVersion || "unknown"} ` +
        `but installer installed ${expectedVersion}. Remove or update the earlier PATH entry that shadows Team Agent.`,
    );
  }
  return { entry, version: actualVersion };
}

export function parseTeamAgentVersion(output) {
  const text = String(output || "").trim();
  const prefixed = text.match(/^team-agent\s+([^\s]+)$/m);
  if (prefixed) {
    return prefixed[1];
  }
  const bare = text.match(/^([0-9]+\.[0-9]+\.[0-9]+(?:[-+][^\s]+)?)$/m);
  return bare ? bare[1] : null;
}

function shadowingPathEntries(searchPath, home, binDir) {
  const entries = uniquePathEntries(searchPath, home);
  const installedIndex = entries.findIndex((entry) => path.resolve(entry) === path.resolve(binDir));
  if (installedIndex === -1) {
    return entries;
  }
  return entries.slice(0, installedIndex);
}

function pathShadowRepairCandidates(searchPath, home, binDir) {
  const candidates = [];
  const seen = new Set();
  const add = (dir, source) => {
    const resolved = path.resolve(expandHomeFor(dir, home));
    if (seen.has(resolved)) {
      return;
    }
    seen.add(resolved);
    candidates.push({ dir: resolved, source });
  };
  for (const entry of shadowingPathEntries(searchPath, home, binDir)) {
    add(entry, "path-before-install");
  }

  const homeLocalBin = path.join(home, ".local", "bin");
  if (!sameFile(homeLocalBin, binDir)) {
    add(homeLocalBin, "home-local-bin");
  }
  return candidates;
}

function isExecutableFile(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

function sameFile(left, right) {
  try {
    return fs.realpathSync(left) === fs.realpathSync(right);
  } catch {
    return path.resolve(left) === path.resolve(right);
  }
}

function uniquePathEntries(searchPath, home) {
  const seen = new Set();
  const entries = [];
  for (const raw of searchPath.split(path.delimiter)) {
    if (!raw) {
      continue;
    }
    const resolved = path.resolve(expandHomeFor(raw, home));
    if (seen.has(resolved)) {
      continue;
    }
    seen.add(resolved);
    entries.push(resolved);
  }
  return entries;
}

function isVersionManagedPath(dir) {
  const value = dir.replace(/\\/g, "/");
  return [
    "/.nvm/versions/",
    "/Cellar/",
    "/volta/tools/image/",
    "/fnm/node-versions/",
    "/.asdf/installs/",
    "/node_modules/.bin",
    "/.npm/_npx/",
    "/_npx/",
  ].some((marker) => value.includes(marker));
}

function isReasonableUserBinDir(dir, home) {
  const relative = path.relative(home, dir);
  return Boolean(relative && !relative.startsWith("..") && !path.isAbsolute(relative));
}

function canWriteDir(dir) {
  try {
    const probe = path.join(dir, `.team-agent-write-test-${process.pid}-${Date.now()}`);
    fs.writeFileSync(probe, "");
    fs.rmSync(probe, { force: true });
    return true;
  } catch {
    return false;
  }
}

function hasForeignWrapper(binDir) {
  return WRAPPER_NAMES.some((name) => {
    const file = path.join(binDir, name);
    return fs.existsSync(file) && !isInstallerManagedWrapper(file);
  });
}

function isInstallerManagedWrapper(file) {
  try {
    const text = fs.readFileSync(file, "utf8");
    if (text.includes(WRAPPER_MARKER)) {
      return true;
    }
    return /^#!\/usr\/bin\/env sh\nexec '[^']+\/bin\/team-agent'(?: '[^']+')? "\$@"\n$/.test(text);
  } catch {
    return false;
  }
}

function ensureBinDirOnShellRc(binDir, options = {}) {
  const env = options.env || process.env;
  const home = options.home || os.homedir();
  const shell = path.basename(env.SHELL || "");
  const rc = shellRcTargets(shell, home);
  if (!rc) {
    return { files: [], skipped: [], unsupported: true };
  }
  const block = rc.style === "fish"
    ? `\n${PATH_MARKER}\nset -gx PATH ${shellQuote(binDir)} $PATH\n`
    : `\n${PATH_MARKER}\nexport PATH=\"${escapeDoubleQuoted(binDir)}:$PATH\"\n`;
  const files = [];
  const skipped = [];
  for (const file of rc.files) {
    let existing = "";
    try {
      existing = fs.readFileSync(file, "utf8");
    } catch {
      existing = "";
    }
    if (existing.includes(PATH_MARKER)) {
      skipped.push(file);
      continue;
    }
    try {
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.appendFileSync(file, block);
      files.push(file);
    } catch {
      continue;
    }
  }
  return { files, skipped, unsupported: false };
}

function shellRcTargets(shell, home) {
  if (shell === "zsh") {
    return { files: [path.join(home, ".zshrc")], style: "posix" };
  }
  if (shell === "bash") {
    return { files: [path.join(home, ".bashrc"), path.join(home, ".bash_profile")], style: "posix" };
  }
  if (shell === "fish") {
    return { files: [path.join(home, ".config", "fish", "config.fish")], style: "fish" };
  }
  if (!shell || shell === "sh") {
    return {
      files: [process.platform === "darwin" ? path.join(home, ".zshrc") : path.join(home, ".profile")],
      style: "posix",
    };
  }
  return null;
}

function installedBinDir(runtimeRoot, opts) {
  const manifest = readInstallManifest(runtimeRoot);
  if (typeof manifest?.binDir === "string" && manifest.binDir) {
    return manifest.binDir;
  }
  const prefix = path.resolve(expandHome(opts.prefix || path.join(os.homedir(), ".local")));
  return path.join(prefix, "bin");
}

function installManifestPath(runtimeRoot) {
  return path.join(runtimeRoot, INSTALL_MANIFEST);
}

export function readInstallManifest(runtimeRoot) {
  try {
    return JSON.parse(fs.readFileSync(installManifestPath(runtimeRoot), "utf8"));
  } catch {
    return null;
  }
}

export function writeInstallManifest(runtimeRoot, manifest) {
  fs.mkdirSync(runtimeRoot, { recursive: true });
  const file = installManifestPath(runtimeRoot);
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(manifest, null, 2)}\n`);
  fs.renameSync(tmp, file);
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

function writeExecWrapper(file, binary, fixedArgs, options = {}) {
  if (fs.existsSync(file) && !options.allowForeign && !isInstallerManagedWrapper(file)) {
    throw new Error(`refusing to overwrite non-Team Agent installer wrapper: ${file}`);
  }
  const args = fixedArgs.map(shellQuote).join(" ");
  const argPrefix = args ? `${args} ` : "";
  const content = `#!/usr/bin/env sh
${WRAPPER_MARKER}
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
  return expandHomeFor(value, os.homedir());
}

function expandHomeFor(value, home) {
  if (value === "~") {
    return home;
  }
  if (value.startsWith("~/")) {
    return path.join(home, value.slice(2));
  }
  return value;
}

function escapeDoubleQuoted(value) {
  return String(value).replace(/["\\`$]/g, "\\$&");
}

function shellQuote(value) {
  return `'${String(value).replace(/'/g, "'\\''")}'`;
}
