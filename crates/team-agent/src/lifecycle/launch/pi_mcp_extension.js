// The Rust materializer prepends teamMcpDefinition and teamMcpReceiptRoot as JSON.
// No package discovery, private extension events, settings writes or factory calls.
import { createHash, randomUUID } from "node:crypto";
import { chmodSync, createReadStream, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, statSync, unlinkSync, watch, writeFileSync } from "node:fs";
import { createInterface } from "node:readline";
import { isAbsolute, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// The sender atomically publishes a ready record only after storing the inbox
// entry. Do not touch the editor or Pi's steer/follow-up queues, even if a run
// starts between isIdle() and prompt preflight: omitting deliverAs fails closed.
async function installLeaderDelivery(pi) {
  const env = teamMcpDefinition.env;
  const owner = process.env.TEAM_AGENT_LEADER_SESSION_UUID;
  if (env.TEAM_AGENT_ID !== "leader" || !owner || typeof pi.sendUserMessage !== "function") return;
  // The extension loader maps this public SDK to the running host, not the
  // next executable on PATH. Older hosts may not expose this module at all.
  let version;
  try { ({ VERSION: version } = await import("@earendil-works/pi-coding-agent")); } catch { return; }
  if (typeof version !== "string" || !/^1\.6\.(0|[1-9]\d*)$/.test(version)) return;
  const dir = join(teamMcpReceiptRoot, `native-${createHash("sha256").update(owner).digest("hex")}`);
  const accepted = join(dir, "accepted");
  const capability = join(dir, "capability.json"), generation = randomUUID();
  let watcher, bindingTimer, ctx, current, confirmation, dispatchDeadline;
  const spoolName = /^\d{20}-[a-f0-9]{64}\.(ready|held)$/;
  const matches = (text, message) => message?.role === "user" && message.content?.length === 1 && message.content[0].text === text;

  // Verify the native JSONL, not just a message event (Pi may not flush a new
  // file until its first reply). Only scan bytes appended after submission.
  async function reconcile(item) {
    if (!item.file || ctx?.sessionManager.getSessionFile() !== item.file) return false;
    let stream;
    try {
      stream = createReadStream(item.file, { start: item.offset });
      for await (const line of createInterface({ input: stream, crlfDelay: Infinity })) {
        let entry;
        try { entry = JSON.parse(line); } catch { continue; }
        if (entry.type !== "message" || !entry.id || !matches(item.text, entry.message)) continue;
        const tmp = join(accepted, `${item.key}.${randomUUID()}.tmp`);
        writeFileSync(tmp, JSON.stringify({id:item.id, entry:entry.id, session:item.session}), {flag:"wx", mode:0o600});
        renameSync(tmp, join(accepted, item.key));
        unlinkSync(join(dir, `${item.key}.claim`));
        return true;
      }
    } catch (error) { if (error.code !== "ENOENT") throw error; }
    finally { stream?.destroy(); }
    return false;
  }

  function drain() {
    if (!ctx || current || !ctx.isIdle()) return;
    const names = readdirSync(dir).filter((f) => spoolName.test(f) && f.endsWith(".ready")).sort();
    if (!names.length) return;
    const receiver = activeReceiver();
    if (!receiver) return;
    let request;
    const name = names.find((name) => {
      request = JSON.parse(readFileSync(join(dir, name), "utf8"));
      return request.epoch === receiver.owner_epoch && request.pane === receiver.pane_id;
    });
    if (!name) return;
    const session = ctx.sessionManager.getSessionId();
    const file = ctx.sessionManager.getSessionFile();
    if (!file) return; // no persistent transcript => no verifiable receipt
    let offset = 0;
    try { offset = statSync(file).size; } catch (error) { if (error.code !== "ENOENT") throw error; }
    const key = name.slice(0, -6);
    const item = {key, session, file, offset, id:request.id, text:`${request.text}\n\n[Team Agent delivery: ${key}]`};
    // A crash beyond this point is uncertain. Never replay a claim blindly.
    renameSync(join(dir, name), join(dir, `${key}.claim`));
    current = item;
    // The extension API returns void, including on asynchronous preflight rejection.
    // A call which never starts a turn will never produce an agent_settled event.
    dispatchDeadline = setTimeout(async () => {
      if (current === item && ctx?.isIdle()) { await retire(item); wake(); }
    }, 5000);
    dispatchDeadline.unref();
    try { pi.sendUserMessage(item.text); }
    catch (error) { abandon(item); setImmediate(wake); throw error; }
  }
  function wake() {
    if (!ctx) return;
    try { drain(); } catch (error) { console.error(`Pi inbox pending: ${error.code ?? "delivery unavailable"}`); }
  }
  function confirm() {
    const item = current;
    if (!item) return;
    return confirmation ??= reconcile(item).then((found) => {
      if (found && current === item) { clearTimeout(dispatchDeadline); current = undefined; }
    }).catch((error) => console.error(`Pi inbox confirmation pending: ${error.code ?? "transcript unavailable"}`))
      .finally(() => { confirmation = undefined; });
  }
  function abandon(item) {
    if (!item || current !== item) return;
    clearTimeout(dispatchDeadline);
    current = undefined;
    // Unconfirmed does not mean unsubmitted. Fence this record against replay,
    // without blocking later messages in this session or a replacement one.
    try { renameSync(join(dir, `${item.key}.claim`), join(dir, `${item.key}.uncertain`)); }
    catch (error) { console.error(`Pi inbox claim retained: ${error.code}`); }
  }
  async function retire(item) {
    if (!item || current !== item) return;
    await confirm();
    if (current === item) await confirm(); // message_end may have preceded Pi's final flush
    abandon(item);
  }
  function activeReceiver() {
    const state = JSON.parse(readFileSync(join(env.TEAM_AGENT_WORKSPACE, ".team/runtime/state.json"), "utf8"));
    const receiver = (state.teams?.[env.TEAM_AGENT_OWNER_TEAM_ID] ?? state).leader_receiver;
    if (receiver?.leader_session_uuid === owner && receiver.status === "attached" &&
        receiver.pane_id === process.env.TMUX_PANE && receiver.owner_epoch > 0) return receiver;
  }
  function advertise(delay = 200) {
    // claim_leader can publish its state just after session_start. Back off
    // while unbound, but still recognize a late claim without restarting Pi.
    try {
      const receiver = activeReceiver();
      if (receiver?.pane_birth_identity) {
        const tmp = `${capability}.${randomUUID()}.tmp`;
        writeFileSync(tmp, JSON.stringify({protocol:1, version, owner, generation, epoch:receiver.owner_epoch,
          pane:receiver.pane_id, carrierBirth:receiver.pane_birth_identity, pid:process.pid}), {flag:"wx", mode:0o600});
        renameSync(tmp, capability);
        return;
      }
    } catch { /* No handshake: sender continues using direct_tmux. */ }
    bindingTimer = setTimeout(() => advertise(Math.min(30_000, delay * 2)), delay);
    bindingTimer.unref();
  }
  pi.on("session_start", (_event, context) => {
    if (typeof context.isIdle !== "function" || typeof context.sessionManager?.getSessionFile !== "function") return;
    ctx = context;
    mkdirSync(accepted, {recursive:true, mode:0o700});
    watcher = watch(dir, (_event, name) => { if (!name || name.endsWith(".ready")) wake(); });
    watcher.unref();
    advertise();
    wake();
  });
  pi.on("message_end", () => { if (current) void confirm(); });
  pi.on("agent_settled", (event) => {
    if (!ctx) return;
    // Freeze just the snapshot pending at Escape, not the receiver. New ready
    // records remain eligible; a normal settlement releases the frozen cohort.
    const [from, to] = event.aborted ? ["ready", "held"] : ["held", "ready"];
    for (const name of readdirSync(dir)) {
      if (spoolName.test(name) && name.endsWith(`.${from}`)) {
        renameSync(join(dir, name), join(dir, `${name.slice(0, -from.length)}${to}`));
      }
    }
    const item = current;
    setImmediate(async () => { await retire(item); wake(); });
  });
  pi.on("session_shutdown", async () => {
    clearTimeout(bindingTimer);
    watcher?.close();
    try { if (JSON.parse(readFileSync(capability, "utf8")).generation === generation) unlinkSync(capability); }
    catch { /* no capability to withdraw */ }
    clearTimeout(dispatchDeadline);
    const retiring = retire(current); // take a last transcript reading using the old context
    ctx = undefined;
    await retiring;
  });
}

export default function teamAgentMcp(pi) {
  const deliveryReady = installLeaderDelivery(pi);
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
    const examples = ["send_message", "get_team_status"].map((tool) => {
      const args = tool === "send_message" ? { to: "leader", content: "..." } : {};
      return surface === "direct"
        ? `- ${tool}: mcp__${alias}__${tool}; arguments ${JSON.stringify(args)}`
        : `- ${tool}: mcp(${JSON.stringify({ tool: `${alias}_${tool}`, args })})`;
    });
    const base = wireSuffix && event.systemPrompt.endsWith(wireSuffix)
      ? event.systemPrompt.slice(0, -wireSuffix.length) : event.systemPrompt;
    wireSuffix = "\n\n# Team Agent MCP wire binding\n"
      + "Use send_message exclusively for communication, using the binding below. Status lookup is optional. Do not pass sender, task_id or schema_version.\n"
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
  return deliveryReady;
}
