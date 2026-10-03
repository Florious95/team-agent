import { writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const [wrapperPath, receiptPath, cwd] = process.argv.slice(2);
if (!wrapperPath || !receiptPath || !cwd) throw new Error("expected wrapper, receipt, and cwd");

const registrations = [];
const unregistrations = [];
const handlers = new Map();
const extensionCommands = [{
  name: "mcp",
  source: "extension",
  sourceInfo: { path: "builtin:mcp", source: "builtin", scope: "user", origin: "top-level" },
}];
const pi = {
  on(event, handler) {
    const list = handlers.get(event) ?? [];
    list.push(handler);
    handlers.set(event, list);
  },
  getCommands() {
    return extensionCommands;
  },
  getMcpServers() {
    return registrations.map(({ name, config }) => ({ name, config, extensionPath: "<native-n1-fixture>" }));
  },
  getAllTools() {
    return registrations.flatMap(({ name, config }) => Object.entries(config.toolExposure ?? {})
      .map(([tool, exposure]) => ({ name: `mcp__${name}__${tool}`, namespace: { name: `mcp__${name}` }, exposure })));
  },
  getActiveTools() {
    return this.getAllTools().filter((tool) => tool.exposure !== "hidden").map((tool) => tool.name);
  },
  registerMcpServer(name, config) {
    registrations.push({ name, config });
  },
  unregisterMcpServer(name) {
    unregistrations.push(name);
  },
};

const extension = await import(pathToFileURL(wrapperPath).href);
if (typeof extension.default !== "function") throw new Error("generated wrapper has no default extension factory");
await extension.default(pi);
const context = {
  cwd,
  sessionManager: {
    getSessionFile: () => undefined,
    getLeafId: () => undefined,
    getEntries: () => [],
  },
};
for (const handler of handlers.get("session_start") ?? []) {
  await handler({ type: "session_start" }, context);
}
if (registrations.length !== 1) throw new Error(`expected one native MCP registration, saw ${registrations.length}`);
const expectedTools = Object.keys(registrations[0].config.toolExposure ?? {})
  .map((tool) => `mcp__${registrations[0].name}__${tool}`);
const deadline = Date.now() + 3_000;
while (!expectedTools.every((name) => pi.getActiveTools().includes(name))) {
  if (Date.now() >= deadline) throw new Error(`native MCP tools never became active: ${pi.getActiveTools()}`);
  await new Promise((resolve) => setTimeout(resolve, 10));
}
const beforeAgentStart = handlers.get("before_agent_start")?.[0];
if (typeof beforeAgentStart !== "function") throw new Error("generated wrapper omitted before_agent_start wire binding");
const binding = await beforeAgentStart({ systemPrompt: "N1 fixture" }, context);
const wirePrompt = binding?.systemPrompt;
if (typeof wirePrompt !== "string" || !expectedTools.every((name) => wirePrompt.includes(name))) {
  throw new Error(`native Team tool wire binding is incomplete: ${wirePrompt}`);
}
await writeFile(receiptPath, `${JSON.stringify({ registrations, unregistrations, activeTools: pi.getActiveTools(), expectedTools, wirePrompt }, null, 2)}\n`, "utf8");
