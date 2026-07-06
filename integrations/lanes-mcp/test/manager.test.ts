import test from "node:test";
import assert from "node:assert/strict";
import os from "node:os";
import { LaneManager, type WaitResult } from "../src/manager.js";
import { fakeResolver, until } from "./helpers.js";

function makeManager(opts: { stallThresholdMs?: number; sessionTtlMs?: number } = {}) {
  return new LaneManager({
    resolveSpec: fakeResolver,
    disableReaper: true,
    baseRepo: os.tmpdir(),
    stallThresholdMs: opts.stallThresholdMs ?? 300_000,
    sessionTtlMs: opts.sessionTtlMs ?? 600_000,
  });
}

async function waitTerminal(m: LaneManager, id: string, timeoutMs = 5000): Promise<WaitResult> {
  const deadline = Date.now() + timeoutMs;
  let last!: WaitResult;
  while (Date.now() < deadline) {
    last = await m.wait(id, 200);
    if (last.status !== "running") return last;
  }
  return last;
}

test("dispatch start + wait completes with final_message = prompt", async () => {
  const m = makeManager();
  try {
    const { id } = await m.dispatchStart({ lane: "codex", prompt: "hello-lane", cwd: os.tmpdir() });
    const r = await waitTerminal(m, id);
    assert.equal(r.status, "done");
    assert.equal(r.final_message, "hello-lane");
  } finally {
    await m.shutdown();
  }
});

test("plan events project into status + touched_files from tool locations", async () => {
  const m = makeManager();
  try {
    const { id } = await m.dispatchStart({ lane: "grok", prompt: "PLAN please", cwd: os.tmpdir() });
    const r = await waitTerminal(m, id);
    assert.equal(r.status, "done");
    assert.ok(r.plan_final, "plan_final present");
    assert.equal(r.plan_final!.total, 3);
    assert.equal(r.plan_final!.completed, 1);
    assert.equal(r.plan_final!.inProgress, 1);
    assert.equal(r.plan_final!.pending, 1);
    assert.equal(r.plan_final!.currentStep, "write the accessor");
    assert.ok(
      (r.touched_files ?? []).some((f) => f.endsWith("planned.txt")),
      `touched_files should include planned.txt, got ${JSON.stringify(r.touched_files)}`,
    );
  } finally {
    await m.shutdown();
  }
});

test("lane_wait returns early on event arrival (long-poll wakes on events)", async () => {
  const m = makeManager();
  try {
    const { id } = await m.dispatchStart({ lane: "opencode", prompt: "SLOW stream", cwd: os.tmpdir() });
    // A 3s-budget wait must return well before the budget because events arrive.
    const t0 = Date.now();
    const first = await m.wait(id, 3000);
    const elapsed = Date.now() - t0;
    assert.ok(elapsed < 2500, `first wait should return early on events, took ${elapsed}ms`);

    let digest = first.digest;
    let r = first;
    while (r.status === "running") {
      r = await m.wait(id, 1000);
      digest += "\n" + r.digest;
    }
    assert.equal(r.status, "done");
    assert.ok(digest.includes("first-chunk-marker"), "digest should carry streamed message text");
    assert.ok(digest.includes("second-chunk-marker"), "digest should carry later message text");
  } finally {
    await m.shutdown();
  }
});

test("lane_wait times out empty and flags suspected_stall on silence", async () => {
  const m = makeManager({ stallThresholdMs: 150 });
  try {
    const { id } = await m.dispatchStart({ lane: "codex", prompt: "STALL now", cwd: os.tmpdir() });
    // Ensure the tool_call event is ingested, then drain all pending digest so
    // the next wait has nothing new to report.
    await until(() => m.status(id).tool_calls >= 1, 4000);
    await m.wait(id, 300);
    // Now the agent is silent; this wait should burn its full window and return running+stalled.
    const t0 = Date.now();
    const r = await m.wait(id, 500);
    const elapsed = Date.now() - t0;
    assert.ok(elapsed >= 400, `silent wait should approach its timeout, took ${elapsed}ms`);
    assert.equal(r.status, "running");
    assert.equal(r.digest, "");
    assert.equal(r.suspected_stall, true);
    assert.ok(r.last_event_age_ms >= 150);
  } finally {
    await m.shutdown();
  }
});

test("lane_cancel maps a cancelled turn to status cancelled", async () => {
  const m = makeManager();
  try {
    const { id } = await m.dispatchStart({ lane: "grok", prompt: "CANCELME long", cwd: os.tmpdir() });
    // Wait until the tool_call event has been ingested.
    await until(() => m.status(id).tool_calls >= 1, 4000);
    await m.cancel(id);
    const r = await waitTerminal(m, id);
    assert.equal(r.status, "cancelled");
  } finally {
    await m.shutdown();
  }
});

test("lane_prompt reuses the session for a second turn", async () => {
  const m = makeManager();
  try {
    const { id } = await m.dispatchStart({ lane: "codex", prompt: "reuse-one", cwd: os.tmpdir() });
    const r1 = await waitTerminal(m, id);
    assert.equal(r1.final_message, "reuse-one");

    await m.promptExisting(id, "reuse-two");
    const r2 = await waitTerminal(m, id);
    assert.equal(r2.status, "done");
    assert.equal(r2.final_message, "reuse-two");

    const entry = m.list().find((e) => e.id === id);
    assert.ok(entry, "run still listed while session alive");
    assert.equal(entry!.turns_count, 2);
    assert.equal(entry!.state, "idle");
  } finally {
    await m.shutdown();
  }
});

test("reaped session rejects lane_prompt", async () => {
  const m = makeManager({ sessionTtlMs: 60 });
  try {
    const { id } = await m.dispatchStart({ lane: "opencode", prompt: "reap-me", cwd: os.tmpdir() });
    await waitTerminal(m, id);
    await new Promise((r) => setTimeout(r, 120));
    const reaped = await m.reap();
    assert.ok(reaped.includes(id), "run should be reaped after idle TTL");
    assert.equal(m.list().find((e) => e.id === id), undefined, "reaped run drops off lane_list");
    await assert.rejects(() => m.promptExisting(id, "too late"), /reaped|not found/);
  } finally {
    await m.shutdown();
  }
});
