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
await new Promise((resolve) => setImmediate(resolve));
if (registrations.length !== 1) throw new Error(`expected one native MCP registration, saw ${registrations.length}`);
await writeFile(receiptPath, `${JSON.stringify({ registrations, unregistrations }, null, 2)}\n`, "utf8");
