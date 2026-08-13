// MCP manager: connect + capability discovery, request round-trips, a
// server-pushed tools/list_changed refresh, and unexpected-drop detection. Backed
// by a real in-memory SDK server through the manager's transport-factory seam.

import { assert, assertEquals } from "@std/assert";
import { mcpManager } from "./manager.ts";
import { mcpRegistry } from "./registry.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import {
  addToolAndNotify,
  clearFakeMcpServers,
  dropFakeMcpServer,
  installFakeMcpServer,
} from "../../tests/helpers/mcp.ts";

function enabledRow(): string {
  return mcpRegistry().create({ name: "Fake", kind: "stdio", command: "fake", enabled: true }).id;
}

// Let any queued microtasks / notification round-trips settle.
function tick(): Promise<void> {
  return new Promise((r) => setTimeout(r, 20));
}

Deno.test("connect discovers capabilities and round-trips a tool call", async () => {
  const env = await setupTestEnv();
  try {
    const id = enabledRow();
    installFakeMcpServer(id, {
      tools: [{ name: "ping" }],
      resources: [{ name: "doc", uri: "file:///d", text: "DATA" }],
    });
    await mcpManager().sync(mcpRegistry().list());

    assertEquals(mcpManager().status(id).status, "connected");
    assertEquals(
      mcpManager()
        .capabilities(id)
        .tools.map((t) => t.name),
      ["ping"],
    );

    const result = (await mcpManager().callTool(id, "ping", {})) as {
      content: { type: string; text: string }[];
    };
    assertStringIncludesContent(result.content, "ping ok");

    const res = await mcpManager().readResource(id, "file:///d");
    assertEquals(res.contents[0].text, "DATA");
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

Deno.test("tools/list_changed refreshes the cached capability list", async () => {
  const env = await setupTestEnv();
  try {
    const id = enabledRow();
    installFakeMcpServer(id, { tools: [{ name: "a" }] });
    await mcpManager().sync(mcpRegistry().list());
    assertEquals(mcpManager().capabilities(id).tools.length, 1);

    addToolAndNotify(id, "b");
    await tick();
    assertEquals(
      mcpManager()
        .capabilities(id)
        .tools.map((t) => t.name)
        .sort(),
      ["a", "b"],
    );
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

Deno.test("an unexpected drop flips status to error", async () => {
  const env = await setupTestEnv();
  try {
    const id = enabledRow();
    installFakeMcpServer(id, { tools: [{ name: "a" }] });
    await mcpManager().sync(mcpRegistry().list());
    assertEquals(mcpManager().status(id).status, "connected");

    await dropFakeMcpServer(id);
    await tick();
    assertEquals(mcpManager().status(id).status, "error");
    assertEquals(mcpManager().capabilities(id).tools.length, 0);
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

function assertStringIncludesContent(
  content: { type: string; text: string }[],
  needle: string,
): void {
  const text = content.map((c) => c.text).join("");
  assert(text.includes(needle), `expected "${text}" to include "${needle}"`);
}
