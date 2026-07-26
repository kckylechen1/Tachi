import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import test from "node:test";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";

test("SDK streamable HTTP transport serves a real request through Hono", async (t) => {
  const serverTransport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
  });
  const mcpServer = new McpServer({
    name: "openclaw-runtime-compat",
    version: "1.0.0",
  });
  await mcpServer.connect(serverTransport);

  let serverError;
  const httpServer = createServer((request, response) => {
    void serverTransport.handleRequest(request, response).catch((error) => {
      serverError = error;
      response.destroy(error);
    });
  });
  httpServer.listen(0, "127.0.0.1");
  await once(httpServer, "listening");

  const address = httpServer.address();
  assert(address && typeof address !== "string");

  t.after(async () => {
    await mcpServer.close();
    httpServer.closeAllConnections();
    await new Promise((resolve, reject) => {
      httpServer.close((error) => (error ? reject(error) : resolve()));
    });
  });

  const response = await fetch(`http://127.0.0.1:${address.port}/mcp`, {
    method: "POST",
    headers: {
      accept: "application/json, text/event-stream",
      "content-type": "application/json",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: {
          name: "openclaw-runtime-compat-client",
          version: "1.0.0",
        },
      },
    }),
  });
  const body = await response.text();

  assert.equal(serverError, undefined);
  assert.equal(response.status, 200, body);
  assert.match(response.headers.get("content-type") ?? "", /^text\/event-stream/);

  const dataLine = body.split("\n").find((line) => line.startsWith("data: "));
  assert(dataLine, `missing MCP data event in response: ${body}`);
  const message = JSON.parse(dataLine.slice("data: ".length));
  assert.equal(message.result.serverInfo.name, "openclaw-runtime-compat");
  assert.equal(message.result.protocolVersion, "2025-06-18");
});
