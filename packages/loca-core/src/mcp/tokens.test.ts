// MCP token resolution: @resource -> fenced reference DATA, slug matching, dedup
// via the claimed set, `#` and `/` never treated as a resource, and the per-block
// cap. `/prompt` tokens are resolved client-side at send (see resolvePrompt), so
// this resolver deliberately leaves them alone.

import { assert, assertEquals, assertStringIncludes } from "@std/assert";
import { flattenPromptMessages, mcpResolveTokens } from "./tokens.ts";
import { mcpRegistry } from "./registry.ts";
import { mcpManager } from "./manager.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { clearFakeMcpServers, installFakeMcpServer } from "../../tests/helpers/mcp.ts";

async function connectFake(spec: Parameters<typeof installFakeMcpServer>[1]): Promise<string> {
  const s = mcpRegistry().create({ name: "Fake", kind: "stdio", command: "fake", enabled: true });
  installFakeMcpServer(s.id, spec);
  await mcpManager().sync(mcpRegistry().list());
  return s.id;
}

Deno.test("resolves @resource as fenced reference DATA", async () => {
  const env = await setupTestEnv();
  try {
    await connectFake({
      resources: [{ name: "My Notes", uri: "mem://notes", text: "secret plan" }],
    });
    // Slug match: "My Notes" -> @my-notes.
    const { block, claimed } = await mcpResolveTokens("see @my-notes please");
    assert(block);
    assertStringIncludes(block, "reference DATA only");
    assertStringIncludes(block, "secret plan");
    assertEquals(claimed.has("my-notes"), true);
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

Deno.test("leaves a /prompt token unresolved (client resolves it at send)", async () => {
  const env = await setupTestEnv();
  try {
    const id = await connectFake({ prompts: [{ name: "commit", text: "Write a commit message" }] });
    mcpRegistry().setPromptEnabled(id, "commit", true);
    // Even enabled, a `/prompt` is not expanded here.
    assertEquals((await mcpResolveTokens("run /commit")).block, null);
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

Deno.test("flattenPromptMessages joins message text and caps the length", () => {
  const flat = flattenPromptMessages([
    { role: "user", content: { text: "line one" } },
    { role: "user", content: [{ text: "a" }, { text: "b" }] },
  ]);
  assertEquals(flat, "line one\na\nb");
  const big = flattenPromptMessages([{ role: "user", content: { text: "x".repeat(100_000) } }]);
  assert(big.length <= 64_000);
});

Deno.test("dedups a repeated token and ignores '#'", async () => {
  const env = await setupTestEnv();
  try {
    await connectFake({ resources: [{ name: "readme", uri: "file:///r", text: "DOC" }] });
    const { block } = await mcpResolveTokens("@readme then @readme again, not #readme");
    assert(block);
    // One block only despite two @readme mentions.
    assertEquals(block.split("BEGIN RESOURCE").length - 1, 1);
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});

Deno.test("caps a large resource body", async () => {
  const env = await setupTestEnv();
  try {
    await connectFake({
      resources: [{ name: "big", uri: "file:///big", text: "x".repeat(100_000) }],
    });
    const { block } = await mcpResolveTokens("@big");
    assert(block);
    // The body is sliced to 64k; the surrounding fence adds a small constant.
    assert(block.length < 65_000, `block too long: ${block.length}`);
  } finally {
    await clearFakeMcpServers();
    await env.teardown();
  }
});
