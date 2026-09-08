#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const binary = process.env.TEAM_AGENT_BINARY;
if (!binary) {
  throw new Error("TEAM_AGENT_BINARY is required");
}
const root = path.resolve(
  process.env.RELEASE_ACCEPTANCE_ROOT ||
    fs.mkdtempSync(path.join(os.tmpdir(), "team-agent-release-status-")),
);
fs.mkdirSync(root, { recursive: true });

function fail(message) {
  throw new Error(message);
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function writeExecutable(file, body) {
  fs.writeFileSync(file, body, { mode: 0o755 });
  fs.chmodSync(file, 0o755);
}

function writeState(workspace, endpoint) {
  fs.mkdirSync(path.join(workspace, ".team", "runtime"), { recursive: true });
  fs.writeFileSync(
    path.join(workspace, ".team", "runtime", "state.json"),
    `${JSON.stringify(
      {
        session_name: "team-demo",
        tmux_endpoint: endpoint,
        tmux_socket: endpoint,
        active_team_key: "demo",
        agents: {
          worker: {
            status: "running",
            provider: "pi",
            agent_id: "worker",
            window: "worker",
            layout_window: "worker",
            pane_id: "%7",
            display: {
              backend: "adaptive",
              status: "opened",
              window: "worker",
              pane_id: "%7",
              target_worker_session: "team-demo",
              linked_session: null,
              display_session: null,
            },
          },
        },
      },
      null,
      2,
    )}\n`,
  );
}

function snapshot(rootDir) {
  const result = new Map();
  function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(file);
      } else if (entry.isFile()) {
        result.set(
          path.relative(rootDir, file),
          crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex"),
        );
      }
    }
  }
  walk(rootDir);
  return result;
}

function assertSameSnapshot(before, after, label) {
  if (before.size !== after.size) fail(`${label} changed workspace file count`);
  for (const [file, digest] of before) {
    if (after.get(file) !== digest) fail(`${label} changed workspace file ${file}`);
  }
}

function runStatus(workspace, home, pathValue, jsonOutput) {
  const args = ["status", "--workspace", workspace];
  if (jsonOutput) args.splice(1, 0, "--json");
  const result = spawnSync(binary, args, {
    env: { ...process.env, HOME: home, PATH: pathValue },
    encoding: "utf8",
    timeout: 5000,
  });
  if (result.error || result.status !== 0) {
    const detail = (result.stderr || result.stdout || "").trim().split(/\r?\n/)[0] || "no output";
    fail(`status ${jsonOutput ? "json" : "human"} failed: ${detail}`);
  }
  return jsonOutput ? JSON.parse(result.stdout) : result.stdout;
}

function assertProjection(value, runtimeStatus) {
  if (!Array.isArray(value?.nodes) || value.nodes.length !== 1) {
    fail("status JSON did not return exactly one node");
  }
  const node = value.nodes[0];
  const expected = [
    "activity",
    "health",
    "name",
    "provider",
    "runtime_status",
    "session_name",
    "tmux_command",
  ];
  const actual = Object.keys(node).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    fail(`status projection keys mismatch: ${actual.join(",")}`);
  }
  if (node.runtime_status !== runtimeStatus) {
    fail(`expected runtime_status=${runtimeStatus}, got ${node.runtime_status}`);
  }
  return node;
}

function assertHumanProjection(text) {
  for (const field of [
    "name:",
    "provider:",
    "runtime_status:",
    "activity:",
    "health:",
    "session_name:",
    "tmux_command:",
  ]) {
    if (!text.includes(field)) fail(`human status omitted ${field}`);
  }
}

const workspace = path.join(root, "workspace");
const home = path.join(root, "home");
const unboundDir = path.join(root, "unbound");
const boundDir = path.join(root, "bound");
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(unboundDir, { recursive: true });
fs.mkdirSync(boundDir, { recursive: true });

const endpoint = `/tmp/team-agent-release-status-${process.pid}.sock`;
writeState(workspace, endpoint);

const unboundMarker = path.join(root, "unbound-spawned");
const unboundBinary = path.join(unboundDir, "nodeprobe");
writeExecutable(
  unboundBinary,
  `#!/bin/sh\nprintf spawned >> ${shellQuote(unboundMarker)}\n`,
);
const unboundBefore = snapshot(workspace);
const unboundPath = unboundDir;
const unknown = runStatus(workspace, home, unboundPath, true);
const unknownNode = assertProjection(unknown, "unknown");
if (unknownNode.tmux_command !== null) fail("unknown status unexpectedly had a tmux command");
assertHumanProjection(runStatus(workspace, home, unboundPath, false));
assertSameSnapshot(unboundBefore, snapshot(workspace), "unbound status");
if (fs.existsSync(unboundMarker)) fail("unbound nodeprobe was spawned");

const boundMarker = path.join(root, "bound-spawned");
const boundBinary = path.join(boundDir, "nodeprobe");
const report = {
  schema_version: 1,
  socket: endpoint,
  sampled_at: "2026-01-01T00:00:00Z",
  nodes: [
    {
      socket: endpoint,
      workspace_path: "/release-status-workspace",
      project_name: "demo",
      session: "team-demo",
      window_index: 1,
      window_name: "worker",
      pane_id: "%7",
      name: "worker",
      provider: "pi",
      state: "idle",
      activity: "idle",
      session_name: "pi-session",
      health: "normal",
      background_tasks: { running: 0 },
      evidence: { method: "pi_activity_channel", detail: "channel" },
    },
  ],
};
writeExecutable(
  boundBinary,
  `#!/bin/sh\nprintf spawned >> ${shellQuote(boundMarker)}\nprintf '%s\\n' ${shellQuote(JSON.stringify(report))}\n`,
);
const targetByPlatform = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
};
const target = targetByPlatform[`${process.platform}-${process.arch}`];
if (!target) fail(`unsupported acceptance host ${process.platform}/${process.arch}`);
const digest = crypto.createHash("sha256").update(fs.readFileSync(boundBinary)).digest("hex");
fs.writeFileSync(
  `${boundBinary}.capability.json`,
  `${JSON.stringify(
    {
      schema: "nodeprobe-capability-v1",
      binary: "nodeprobe",
      binary_sha256: digest,
      source_repo: "Florious95/team-agent-scratch/nodeprobe",
      source_commit: "ff316dc0afe8ab280e61d30934e7624579be6224",
      source_tree: "5217a41aa914ddcb72c27f39f1b4af9ead68b1b6",
      target,
      report_schema: 1,
      capabilities: ["tmux.list-panes", "ps.pid_ppid_stat_comm"],
      forbidden: ["tmux.capture-pane", "tmux.attach", "tmux.send-keys", "process.argv", "pane_body"],
    },
    null,
    2,
  )}\n`,
);
const boundBefore = snapshot(workspace);
const running = runStatus(workspace, home, boundDir, true);
const runningNode = assertProjection(running, "running");
if (runningNode.activity !== "idle" || runningNode.health !== "normal") {
  fail("bound status did not preserve nodeprobe activity/health");
}
if (runningNode.session_name !== "pi-session" || !runningNode.tmux_command) {
  fail("bound status did not project session/tmux command");
}
assertHumanProjection(runStatus(workspace, home, boundDir, false));
assertSameSnapshot(boundBefore, snapshot(workspace), "bound status");
if (!fs.existsSync(boundMarker)) fail("bound nodeprobe was not spawned");

console.log("release installer/status acceptance: PASS");
