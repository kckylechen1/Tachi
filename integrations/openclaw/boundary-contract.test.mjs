import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, mock, test } from "node:test";
import { Value } from "@sinclair/typebox/value";
import { MemoryMcpClient } from "./mcp-client.js";
import { registerNativeTachiMemoryCapability } from "./native-memory.js";

const testHome = fs.mkdtempSync(path.join(os.tmpdir(), "tachi-openclaw-contract-"));
const originalEnv = Object.fromEntries(
  ["TACHI_HOME", "MEMORY_BRIDGE_CAPTURE_MIN_CHARS", "TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS"]
    .map((key) => [key, process.env[key]]),
);
process.env.TACHI_HOME = testHome;
process.env.MEMORY_BRIDGE_CAPTURE_MIN_CHARS = "200";
process.env.TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS = "false";

// Import the real config without loading the developer's machine-local env files.
const readFileSync = fs.readFileSync;
const envFiles = new Set([
  path.join(os.homedir(), ".secrets", "master.env"),
  path.join(testHome, "config.env"),
]);
const envReadMock = mock.method(fs, "readFileSync", (file, ...args) =>
  envFiles.has(file) ? "" : readFileSync(file, ...args),
);
const { bridgeConfigSchema, defaultConfig } = await import("./config.js");
const { memoryHybridBridgePlugin } = await import("./index.js");
envReadMock.mock.restore();

after(() => {
  for (const [key, value] of Object.entries(originalEnv)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(testHome, { recursive: true, force: true });
});

function clientWithResponse(payload) {
  const calls = [];
  const client = new MemoryMcpClient("/unused/global.db", "/unused/project.db");
  // Replace only transport access; public methods and response conversion are real.
  client.client = {
    async callTool(request) {
      calls.push(request);
      return { content: [], structuredContent: payload };
    },
  };
  return { client, calls };
}

function pluginApi(pluginConfig = {}) {
  const tools = new Map();
  const api = {
    pluginConfig: {
      dbPath: path.join(testHome, "project.db"),
      globalDbPath: path.join(testHome, "global.db"),
      ...pluginConfig,
    },
    logger: { info() {}, warn() {}, error() {} },
    registerTool(tool) { tools.set(tool.name, tool); },
    registerService() {},
    registerMemoryCapability(capability) { api.memoryCapability = capability; },
    on() {},
  };
  return { api, tools };
}

test("memory response conversion preserves zero importance and defaults only invalid values", async () => {
  for (const [value, expected] of [
    [0, 0], ["0", 0], [0.4, 0.4], ["0.4", 0.4],
    [undefined, 0.7], [null, 0.7], ["", 0.7], ["  ", 0.7],
    ["bad", 0.7], [NaN, 0.7], [Infinity, 0.7], [false, 0.7],
    [true, 0.7], [[], 0.7], [{}, 0.7],
  ]) {
    const { client } = clientWithResponse({ id: "memory-1", importance: value });
    const entry = await client.getMemory("memory-1");
    assert.equal(entry.importance, expected, `importance: ${String(value)}`);
  }
});

test("search and read preserve cleaned source refs without mutating input metadata", async () => {
  const good = { ref_type: "message", ref_id: "turn-1", note: "kept" };
  const metadata = { source_refs: [null, good, "bad", {}, { ref_type: "file" }], tag: "retained" };
  const snapshot = structuredClone(metadata);
  const row = { id: "memory-1", importance: 0, metadata };
  const { client } = clientWithResponse({ results: [row] });
  const { docs } = await client.searchMemory("query");
  assert.deepEqual(docs[0].metadata, { source_refs: [good], tag: "retained" });
  assert.deepEqual(metadata, snapshot);

  for (const source_refs of [null, "not an array", {}, undefined]) {
    const { client: reader } = clientWithResponse({ id: "memory-1", metadata: { source_refs, tag: "kept" } });
    assert.deepEqual((await reader.getMemory("memory-1")).metadata, { source_refs: [], tag: "kept" });
  }
});

test("config counts honor manifest bounds and retain valid numeric strings", () => {
  const manifest = JSON.parse(fs.readFileSync(new URL("./openclaw.plugin.json", import.meta.url), "utf8"));
  for (const [name, maximum] of [["topK", 20], ["captureMinChars", 10000]]) {
    assert.deepEqual(manifest.configSchema.properties[name], { type: "integer", minimum: 1, maximum });
    for (const value of [1, maximum, "1", String(maximum), " 2 "]) {
      assert.equal(bridgeConfigSchema.parse({ [name]: value })[name], Number(value));
    }
    for (const value of [0, -1, maximum + 1, 1.5, "1.5", String(maximum + 1), "bad", "", NaN, Infinity, null, true, false, [], [2], {}]) {
      assert.equal(bridgeConfigSchema.parse({ [name]: value })[name], defaultConfig[name], `${name}: ${String(value)}`);
    }
  }
});

test("capture threshold environment defaults are bounded integers", async () => {
  try {
    for (const [index, [value, expected]] of [
      ["1", 1], ["10000", 10000], [" 400 ", 400],
      ["0", 200], ["10001", 200], ["1.5", 200], ["bad", 200],
    ].entries()) {
      process.env.MEMORY_BRIDGE_CAPTURE_MIN_CHARS = value;
      const fresh = await import(`./config.js?capture-default-case=${index}`);
      assert.equal(fresh.defaultConfig.captureMinChars, expected);
      assert.equal(fresh.bridgeConfigSchema.parse({ captureMinChars: "bad" }).captureMinChars, expected);
    }
  } finally {
    process.env.MEMORY_BRIDGE_CAPTURE_MIN_CHARS = "200";
  }
});

test("experimental tools require an explicit boolean config before registration", () => {
  const manifest = JSON.parse(fs.readFileSync(new URL("./openclaw.plugin.json", import.meta.url), "utf8"));
  assert.equal(manifest.configSchema.properties.exposeExperimentalTachiTools.type, "boolean");
  for (const enabled of [true, false]) {
    const { api, tools } = pluginApi({ exposeExperimentalTachiTools: enabled });
    memoryHybridBridgePlugin.register(api);
    assert.equal(tools.has("compact_context"), enabled);
    assert.equal(tools.has("memory_search"), true);
  }
  for (const value of ["false", "0", "true", "1", 0, 1, null, [], {}]) {
    const { api, tools } = pluginApi({ exposeExperimentalTachiTools: value });
    assert.throws(() => memoryHybridBridgePlugin.register(api), /exposeExperimentalTachiTools.*boolean/);
    assert.equal(tools.size, 0);
  }
  assert.equal(bridgeConfigSchema.parse({}).exposeExperimentalTachiTools, false);
});

test("experimental environment opt-in stays strict and explicit false overrides it", async () => {
  try {
    for (const [index, [value, enabled]] of [
      ["1", true], ["true", true], ["0", false], ["false", false], ["yes", false],
    ].entries()) {
      process.env.TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS = value;
      const fresh = await import(`./config.js?experimental-default-case=${index}`);
      assert.equal(fresh.bridgeConfigSchema.parse({}).exposeExperimentalTachiTools, enabled);
      assert.equal(fresh.bridgeConfigSchema.parse({ exposeExperimentalTachiTools: false }).exposeExperimentalTachiTools, false);
    }
  } finally {
    process.env.TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS = "false";
  }
});

const invalidCounts = [-1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, "10", true, []];

test("memory_search advertises unsigned integer counts and enforces them before MCP", async (t) => {
  const calls = [];
  t.mock.method(MemoryMcpClient.prototype, "getClient", async () => ({
    async callTool(request) {
      calls.push(request);
      return { content: [], structuredContent: { results: [] } };
    },
  }));
  const { api, tools } = pluginApi({ topK: "7" });
  memoryHybridBridgePlugin.register(api);
  const tool = tools.get("memory_search");
  assert.equal(tool.parameters.properties.maxResults.type, "integer");
  for (const value of [0, 1, 100, 150, Number.MAX_SAFE_INTEGER]) {
    assert.equal(Value.Check(tool.parameters, { query: "test", maxResults: value }), true);
    await tool.execute("test", { query: "test", maxResults: value });
    assert.equal(calls.at(-1).arguments.top_k, value);
  }
  await tool.execute("test", { query: "test" });
  assert.equal(calls.at(-1).arguments.top_k, 7);
  const sent = calls.length;
  for (const value of invalidCounts) {
    assert.equal(Value.Check(tool.parameters, { query: "test", maxResults: value }), false);
    await tool.execute("test", { query: "test", maxResults: value });
    assert.equal(calls.length, sent, `invalid maxResults sent to MCP: ${String(value)}`);
  }
});

test("native memory and direct MCP client reject invalid counts on the shared search path", async () => {
  const { client, calls } = clientWithResponse({ results: [] });
  const { api } = pluginApi();
  registerNativeTachiMemoryCapability({ api, topK: 6, ensureClient: async () => client, resolveAgentId: () => "main" });
  const { manager } = await api.memoryCapability.runtime.getMemorySearchManager({ agentId: "main" });
  await manager.search("query");
  assert.equal(calls.at(-1).arguments.top_k, 6);
  await manager.search("query", { maxResults: 0 });
  assert.equal(calls.at(-1).arguments.top_k, 0);
  await client.searchMemory("query");
  assert.equal(calls.at(-1).arguments.top_k, undefined);
  const sent = calls.length;
  for (const value of invalidCounts) {
    await assert.rejects(manager.search("query", { maxResults: value }), /top_k.*non-negative safe integer/);
    await assert.rejects(client.searchMemory("query", undefined, { top_k: value }), /top_k.*non-negative safe integer/);
  }
  assert.equal(calls.length, sent);
});
