import test from "node:test";
import assert from "node:assert/strict";

import { doctorSelfCheckLine, doctorSelfCheckVerdict } from "./install.mjs";

test("empty doctor workspace is ok for installer self-check", () => {
  const verdict = doctorSelfCheckVerdict(
    JSON.stringify({
      ok: false,
      error: "workspace has no Team Agent spec or runtime context",
      tmux: { installed: true },
      mcp: { server_command: "/x/team_orchestrator" },
    }),
    { status: 1 },
  );

  assert.equal(verdict.kind, "ok");
  assert.deepEqual(verdict.blockers, []);
});

test("real doctor blockers remain blockers", () => {
  const verdict = doctorSelfCheckVerdict(
    JSON.stringify({
      ok: false,
      tmux: { installed: false },
      mcp: { server_command: null },
    }),
    { status: 1 },
  );

  assert.equal(verdict.kind, "blockers");
  assert.ok(verdict.blockers.some((blocker) => blocker.includes("tmux")));
});

test("unparseable or killed doctor self-check is advisory without blocker wording", () => {
  for (const [doctorBody, spawnMeta] of [
    ["", { status: 1 }],
    ["not json", { status: 1 }],
    [JSON.stringify({ ok: false }), { signal: "SIGTERM" }],
  ]) {
    const verdict = doctorSelfCheckVerdict(doctorBody, spawnMeta);
    assert.equal(verdict.kind, "advisory");
    assert.doesNotMatch(doctorSelfCheckLine(verdict), /has blockers/);
  }
});
