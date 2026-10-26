// The Rust leaf prepends `const teamMcp = {key, command, args, env, tools};`.
// It registers only this generation's owned Team server for this invocation:
// no package discovery, settings or receipt writes, and no factory calls.
import { realpathSync, statSync } from "node:fs";
import { isAbsolute } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export default function teamAgentMcp(pi) {
  const { key, tools } = teamMcp;
  const definition = { command: teamMcp.command, args: teamMcp.args, env: teamMcp.env };
  let surface, registration, timer, failure, wireSuffix;
  let ready = false, stopped = false, starting = false, nativeOwned = false;

  // Third-party error text can carry credentials: report only the stage.
  function fail(ctx, stage) {
    clearInterval(timer);
    failure = new Error(`Team Agent MCP unavailable at ${stage}`);
    ctx.abort();
    ctx.shutdown();
    return failure;
  }

  function directNames() {
    const namespace = `mcp__${key}`;
    const exposed = pi.getAllTools().filter((tool) => tool.namespace?.name === namespace && tool.exposure !== "hidden");
    const active = new Set(pi.getActiveTools());
    const names = tools.map((tool) => `mcp__${key}__${tool}`);
    if (exposed.length !== tools.length) return undefined;
    if (!names.every((name) => active.has(name) && exposed.some((tool) => tool.name === name && tool.exposure === "direct"))) return undefined;
    return names;
  }

  function proxyAvailable() {
    const tool = pi.getAllTools().find((tool) => tool.name === "mcp");
    const properties = tool?.parameters?.properties;
    return properties?.tool?.type === "string" && properties?.args && pi.getActiveTools().includes("mcp");
  }

  // A loaded MCP extension that replaced Pi's built-in support may export a
  // public registration function. Import only that export, never its factory.
  async function publicBridge(commands) {
    const own = realpathSync(fileURLToPath(import.meta.url));
    const entries = new Set();
    for (const command of commands) {
      if (command.source !== "extension") continue;
      const entry = command.sourceInfo?.path ?? command.path;
      if (!entry || !isAbsolute(entry)) continue;
      const canonical = realpathSync(entry);
      if (canonical === own) continue;
      if (!statSync(canonical).isFile()) throw new Error("Loaded extension entry is not a regular file");
      entries.add(canonical);
    }
    const bridges = [];
    for (const entry of entries) {
      const module = await import(pathToFileURL(entry).href);
      if (typeof module.registerMcpServer === "function") bridges.push(module.registerMcpServer);
    }
    if (bridges.length > 1) throw new Error("Ambiguous loaded MCP registration capabilities");
    return bridges[0];
  }

  pi.on("session_start", async (_event, ctx) => {
    if (starting || surface || failure || stopped) return;
    starting = true;
    try {
      const native = typeof pi.registerMcpServer === "function" && typeof pi.unregisterMcpServer === "function";
      const commands = typeof pi.getCommands === "function" ? pi.getCommands() : [];
      const builtin = commands.some((command) => command.source === "extension" && command.sourceInfo?.path === "builtin:mcp");
      const deadline = performance.now() + 30_000;
      let loading;
      let bridge;
      try {
        if (!(native && builtin)) {
          bridge = await Promise.race([
            publicBridge(commands),
            new Promise((_resolve, reject) => { loading = setTimeout(() => reject(new Error("timeout")), 30_000); }),
          ]);
        }
      } finally { clearTimeout(loading); }
      if (stopped || failure) return;
      if (bridge) {
        surface = "proxy";
        registration = bridge({ pi, name: key, definition: { ...definition, includeTools: tools } });
        if (!registration || typeof registration.dispose !== "function") throw new Error("no owned dispose handle");
      } else if (native) {
        surface = "direct";
        pi.registerMcpServer(key, { ...definition, exposure: "hidden", toolExposure: Object.fromEntries(tools.map((tool) => [tool, "direct"])) });
        nativeOwned = true;
      } else {
        throw new Error("no MCP registration capability");
      }
      // Never await the connection here: a later session_start handler may own the consumer.
      timer = setInterval(() => {
        if (stopped || failure) return;
        if (performance.now() > deadline) { fail(ctx, "tools-ready-timeout"); return; }
        try {
          if (surface === "direct" ? directNames() : proxyAvailable()) { ready = true; clearInterval(timer); }
        } catch { fail(ctx, "tools-ready"); }
      }, 25);
    } catch { throw fail(ctx, "registration"); }
  });

  async function waitReady() {
    while (!ready && !failure && !stopped) await new Promise((resolve) => setTimeout(resolve, 25));
  }

  // Business input waits for the owned tools; a failed binding ends the run.
  pi.on("input", async (_event, ctx) => {
    await waitReady();
    if (failure || stopped) {
      ctx.shutdown();
      return { action: "handled" };
    }
    return { action: "continue" };
  });

  pi.on("before_agent_start", async (event, ctx) => {
    await waitReady();
    if (failure) throw failure;
    if (stopped || !surface) throw fail(ctx, "wire-binding");
    if (surface === "direct" ? !directNames() : !proxyAvailable()) throw fail(ctx, "wire-binding");
    const examples = tools.map((tool) => {
      const args = tool === "send_message" ? { to: "leader", content: "..." }
        : tool === "report_result" ? { summary: "..." } : {};
      return surface === "direct"
        ? `- ${tool}: mcp__${key}__${tool}; arguments ${JSON.stringify(args)}`
        : `- ${tool}: mcp(${JSON.stringify({ tool: `${key}_${tool}`, args })})`;
    });
    const base = wireSuffix && event.systemPrompt.endsWith(wireSuffix)
      ? event.systemPrompt.slice(0, -wireSuffix.length) : event.systemPrompt;
    wireSuffix = "\n\n# Team Agent MCP wire binding\n"
      + "Use these actual tools for the logical Team Agent operations. Do not pass sender, task_id or schema_version.\n"
      + examples.join("\n");
    return { systemPrompt: base + wireSuffix };
  });

  pi.on("before_provider_request", (_event, ctx) => {
    if (failure || stopped || !ready) ctx.abort();
  });

  pi.on("session_shutdown", async (_event, ctx) => {
    if (stopped) return;
    stopped = true;
    clearInterval(timer);
    const current = registration;
    registration = undefined;
    try {
      if (nativeOwned) {
        nativeOwned = false;
        pi.unregisterMcpServer(key);
      }
      await current?.dispose();
    } catch { throw fail(ctx, "shutdown"); }
  });
}
