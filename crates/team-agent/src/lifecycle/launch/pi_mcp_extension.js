// The Rust materializer prepends teamMcpDefinition and teamMcpReceiptRoot as JSON.
// No package discovery, private extension events, settings writes or factory calls.
import { createHash, randomUUID } from "node:crypto";
import { chmodSync, realpathSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export default function teamAgentMcp(pi) {
  // Keep full UUID entropy and all three native tool names below Pi's 64-character limit.
  const alias = `team_${randomUUID().replaceAll("-", "")}`;
  const wanted = teamMcpDefinition.includeTools;
  const receipt = join(teamMcpReceiptRoot, `${alias}.json`);
  let backend, surface, registration, timer, failure, bindingReady = false, stopped = false, starting = false;
  let wireSuffix;
  let nativeOwned = false;
  const returned = new Set();
  let frame = {
    schema_version: "team-agent.pi-mcp.v1", alias,
    api_shape: Object.fromEntries(["registerMcpServer", "unregisterMcpServer", "getCommands", "getAllTools", "getActiveTools"]
      .map((name) => [name, typeof pi[name]])),
    candidate: teamMcpDefinition.command, tools: wanted,
    workspace: teamMcpDefinition.env.TEAM_AGENT_WORKSPACE,
    agent_id: teamMcpDefinition.env.TEAM_AGENT_ID,
    team_id: teamMcpDefinition.env.TEAM_AGENT_OWNER_TEAM_ID,
  };

  function publish(stage, extra = {}) {
    frame = { ...frame, stage, backend, surface, ...extra };
    const temp = `${receipt}.tmp`;
    let written = false;
    try {
      writeFileSync(temp, JSON.stringify(frame) + "\n", { flag: "wx", mode: 0o600 });
      written = true;
      chmodSync(temp, 0o600);
      renameSync(temp, receipt);
    } catch (error) {
      if (written) {
        try { unlinkSync(temp); } catch { /* only the exact temporary file we created */ }
      }
      throw error;
    }
  }

  function fail(ctx, stage, error) {
    clearInterval(timer);
    // Third-party error text can contain credentials: retain correlation, not its content.
    const errorSha256 = createHash("sha256").update(String(error)).digest("hex");
    failure = new Error(`Pi Team MCP unavailable at ${stage}; receipt ${receipt}`);
    try { publish("Failed", { failed_stage: stage, error_sha256: errorSha256 }); }
    catch { failure = new Error("Pi Team MCP failed and its owned receipt could not be written"); }
    ctx.abort();
    ctx.shutdown();
    return failure;
  }

  function proxyAvailable() {
    const tool = pi.getAllTools().find((tool) => tool.name === "mcp");
    const properties = tool?.parameters?.properties;
    return properties?.tool?.type === "string" && properties?.args
      && pi.getActiveTools().includes("mcp");
  }

  function directBinding() {
    const namespace = `mcp__${alias}`;
    const tools = pi.getAllTools().filter((tool) => tool.namespace?.name === namespace && tool.exposure !== "hidden");
    const active = new Set(pi.getActiveTools());
    const names = wanted.map((name) => `mcp__${alias}__${name}`);
    if (tools.length !== wanted.length) return undefined;
    if (!names.every((name) => active.has(name) && tools.some((tool) => tool.name === name && tool.exposure === "direct"))) return undefined;
    return names;
  }

  async function publicBridge(commands) {
    const ownPath = realpathSync(fileURLToPath(import.meta.url));
    const entries = new Set();
    for (const command of commands) {
      if (command.source !== "extension") continue;
      const entry = command.sourceInfo?.path ?? command.path;
      if (!entry || !isAbsolute(entry)) continue;
      const canonical = realpathSync(entry);
      if (canonical === ownPath) continue;
      if (!statSync(canonical).isFile()) throw new Error("Loaded extension entry is not a regular file");
      entries.add(canonical);
    }
    const bridges = [];
    for (const entry of entries) {
      // Import only the public exports of already-loaded entries, never their default factory.
      const module = await import(pathToFileURL(entry).href);
      if (typeof module.registerMcpServer === "function") bridges.push({ entry, register: module.registerMcpServer });
    }
    if (bridges.length > 1) throw new Error("Ambiguous loaded MCP registration capabilities");
    return bridges[0];
  }

  pi.on("session_start", async (_event, ctx) => {
    if (starting || backend || failure || stopped) return;
    starting = true;
    try {
      publish("ExecutableCatalogReady");
      const native = typeof pi.registerMcpServer === "function" && typeof pi.unregisterMcpServer === "function";
      const commands = typeof pi.getCommands === "function" ? pi.getCommands() : [];
      const builtin = commands.some((command) => command.source === "extension" && command.sourceInfo?.path === "builtin:mcp");
      const deadline = performance.now() + 30_000;
      let loadingTimeout;
      let bridge;
      try {
        if (!(native && builtin)) {
          bridge = await Promise.race([
            publicBridge(commands),
            new Promise((_resolve, reject) => { loadingTimeout = setTimeout(() => reject(new Error("Public MCP capability loading timed out")), 30_000); }),
          ]);
        }
      } finally { clearTimeout(loadingTimeout); }
      if (stopped || failure) return;
      if (bridge) {
        backend = "public-bridge";
        surface = "proxy";
        registration = bridge.register({ pi, name: alias, definition: teamMcpDefinition });
        if (!registration || typeof registration.dispose !== "function") throw new Error("Public MCP registration has no owned dispose handle");
        publish("RegistrationSelected", { bridge_entry: bridge.entry });
      } else if (native) {
        backend = "native-api";
        const { command, args, env, cwd } = teamMcpDefinition;
        pi.registerMcpServer(alias, {
          command, args, env, ...(cwd ? { cwd } : {}),
          exposure: "hidden", toolExposure: Object.fromEntries(wanted.map((name) => [name, "direct"])),
        });
        nativeOwned = true;
        publish("RegistrationSelected");
      } else {
        throw new Error("No native MCP API or loaded public MCP registration bridge");
      }
      // Never await connection here: later session_start handlers may own the consumer.
      timer = setInterval(() => {
        if (stopped || failure) return;
        try {
          if (performance.now() > deadline) {
            fail(ctx, "tools-ready-timeout", new Error("MCP consumer did not expose the registered tools"));
            return;
          }
          const names = backend === "native-api" ? directBinding() : undefined;
          if (names) {
            surface = "direct";
            bindingReady = true;
            clearInterval(timer);
            publish("ToolsAvailable", { actual_tool_names: names, evidence: "pi-runtime-tool-registry" });
          } else if (!builtin && proxyAvailable()) {
            surface = "proxy";
            bindingReady = true;
            clearInterval(timer);
            publish("RegistrationSelected", { evidence: "proxy-present; owned MCP calls still pending" });
          }
        } catch (error) { fail(ctx, "tools-ready", error); }
      }, 25);
    } catch (error) { throw fail(ctx, "registration", error); }
  });

  async function waitForBinding() {
    while (!bindingReady && !failure && !stopped) await new Promise((resolve) => setTimeout(resolve, 25));
  }

  // before_agent_start exceptions are diagnostic in Pi; input must be handled to stop a new run.
  pi.on("input", async (_event, ctx) => {
    await waitForBinding();
    if (failure || stopped) {
      ctx.shutdown();
      return { action: "handled" };
    }
    return { action: "continue" };
  });

  pi.on("before_agent_start", async (event, ctx) => {
    await waitForBinding();
    if (failure) throw failure;
    if (stopped || !backend) throw fail(ctx, "wire-binding", new Error("No owned MCP registration"));
    if (surface === "direct" && !directBinding()) throw fail(ctx, "wire-binding", new Error("Owned MCP tools became unreachable"));
    if (surface === "proxy" && !proxyAvailable()) throw fail(ctx, "wire-binding", new Error("MCP proxy became unreachable"));
    const examples = wanted.map((tool) => {
      const args = tool === "send_message" ? { to: "leader", content: "..." }
        : tool === "report_result" ? { summary: "..." } : {};
      return surface === "direct"
        ? `- ${tool}: mcp__${alias}__${tool}; arguments ${JSON.stringify(args)}`
        : `- ${tool}: mcp(${JSON.stringify({ tool: `${alias}_${tool}`, args })})`;
    });
    const base = wireSuffix && event.systemPrompt.endsWith(wireSuffix)
      ? event.systemPrompt.slice(0, -wireSuffix.length) : event.systemPrompt;
    wireSuffix = "\n\n# Team Agent MCP wire binding\n"
      + "Use these actual tools for the logical Team Agent operations above. Do not pass sender, task_id or schema_version.\n"
      + examples.join("\n");
    return { systemPrompt: base + wireSuffix };
  });

  pi.on("before_provider_request", (_event, ctx) => {
    if (failure || stopped || !bindingReady) ctx.abort();
  });

  pi.on("tool_result", (event, ctx) => {
    if (stopped || failure) return;
    const name = event.toolName === "mcp" ? event.input?.tool : event.toolName;
    const tool = wanted.find((tool) => name === (surface === "direct" ? `mcp__${alias}__${tool}` : `${alias}_${tool}`));
    if (!tool) return;
    if (event.isError === false) returned.add(tool);
    try {
      publish(returned.size === wanted.length ? "ToolsVerified" : frame.stage, {
        returned_tools: [...returned], last_call_is_error: event.isError ?? null,
        evidence: "owned tool-result events; business success requires the original results",
      });
    } catch (error) { throw fail(ctx, "tool-receipt", error); }
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
        pi.unregisterMcpServer(alias);
      }
      await current?.dispose();
      publish(failure ? "Failed" : frame.stage, { shutdown: true });
    } catch (error) { throw fail(ctx, "shutdown", error); }
  });
}
