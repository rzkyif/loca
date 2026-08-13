// MCP registry: CRUD + the assertConnectable enable gate, per-tool/per-prompt
// enablement as JSON sets, and the projections that merge a DB row with the
// manager's live status + capabilities.

import { assertEquals, assertThrows } from "@std/assert";
import { mcpRegistry } from "./registry.ts";
import { mcpManager } from "./manager.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { clearFakeMcpServers, installFakeMcpServer } from "../../tests/helpers/mcp.ts";

Deno.test("create: a disabled draft may omit command/url", async () => {
  const env = await setupTestEnv();
  try {
    const s = mcpRegistry().create({ name: "Draft", kind: "stdio", enabled: false });
    assertEquals(s.enabled, false);
    assertEquals(s.command, undefined);
    assertEquals(s.status, "disconnected");
  } finally {
    await env.teardown();
  }
});

Deno.test("create: enabling without a transport target throws", async () => {
  const env = await setupTestEnv();
  try {
    assertThrows(
      () => mcpRegistry().create({ name: "Bad", kind: "stdio", enabled: true }),
      Error,
      "command",
    );
    assertThrows(
      () => mcpRegistry().create({ name: "Bad", kind: "remote", enabled: true }),
      Error,
      "url",
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("update: can't flip a draft on while still incomplete; rename leaves config", async () => {
  const env = await setupTestEnv();
  try {
    const s = mcpRegistry().create({ name: "Draft", kind: "stdio", enabled: false });
    assertThrows(() => mcpRegistry().update(s.id, { enabled: true }), Error, "command");
    const renamed = mcpRegistry().update(s.id, { name: "Renamed" });
    assertEquals(renamed.name, "Renamed");
    assertEquals(renamed.kind, "stdio");
    assertEquals(renamed.enabled, false);
  } finally {
    await env.teardown();
  }
});

Deno.test("setTool/PromptEnabled: toggles the JSON set idempotently", async () => {
  const env = await setupTestEnv();
  try {
    const s = mcpRegistry().create({ name: "S", kind: "stdio", enabled: false });
    assertEquals(mcpRegistry().setToolEnabled(s.id, "search", true).toolEnabled, ["search"]);
    // Re-enabling the same name is a no-op (set semantics).
    assertEquals(mcpRegistry().setToolEnabled(s.id, "search", true).toolEnabled, ["search"]);
    assertEquals(mcpRegistry().setToolEnabled(s.id, "search", false).toolEnabled, []);
    assertEquals(mcpRegistry().setPromptEnabled(s.id, "commit", true).promptEnabled, ["commit"]);
  } finally {
    await env.teardown();
  }
});

Deno.test("project + listAll*: merge live capabilities and honor enable flags", async () => {
  const env = await setupTestEnv();
  try {
    const s = mcpRegistry().create({
      name: "Fake",
      kind: "stdio",
      command: "fake",
      enabled: true,
    });
    installFakeMcpServer(s.id, {
      tools: [{ name: "search", description: "Find things" }, { name: "write" }],
      prompts: [{ name: "commit", text: "Write a commit" }],
      resources: [{ name: "Readme", uri: "file:///readme", text: "hi" }],
    });
    await mcpManager().sync(mcpRegistry().list());

    const projected = mcpRegistry().getOrThrow(s.id);
    assertEquals(projected.status, "connected");
    assertEquals(projected.toolCount, 2);
    assertEquals(projected.promptCount, 1);
    assertEquals(projected.resourceCount, 1);

    // Only enabled tools surface in the shared Tool list, tagged providerKind.
    mcpRegistry().setToolEnabled(s.id, "search", true);
    const tools = mcpRegistry().listAllTools();
    assertEquals(tools.length, 2);
    const search = tools.find((t) => t.name === "search");
    assertEquals(search?.providerKind, "mcp");
    assertEquals(search?.providerName, "Fake");
    assertEquals(search?.enabled, true);
    assertEquals(tools.find((t) => t.name === "write")?.enabled, false);

    // Prompts carry the user's enable flag; resources are all listed.
    assertEquals(
      mcpRegistry()
        .listPrompts()
        .map((p) => p.name),
      ["commit"],
    );
    assertEquals(
      mcpRegistry()
        .listResources()
        .map((r) => r.uri),
      ["file:///readme"],
    );
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});
