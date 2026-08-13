// In-memory MCP server harness for tests. Backs the manager's transport factory
// with a real `@modelcontextprotocol/sdk` server over a linked in-memory
// transport pair, so connect / capability discovery / callTool / readResource /
// getPrompt / list_changed / drop all exercise the actual SDK rather than a
// hand-rolled stub. Register a fake server for a given server id, then enable a
// matching row and sync the manager.

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { z } from "zod";
import { mcpManager } from "../../src/mcp/manager.ts";

export interface FakeMcpSpec {
  tools?: { name: string; description?: string }[];
  prompts?: { name: string; description?: string; requiredArg?: boolean; text?: string }[];
  resources?: { name: string; uri: string; text?: string; mimeType?: string }[];
}

interface Fake {
  server: McpServer;
  serverTransport: InMemoryTransport;
  clientTransport: InMemoryTransport;
}

const fakes = new Map<string, Fake>();

/** Register an in-memory MCP server for `id` and point the manager's transport
 *  factory at the registered fakes. Returns the SDK server so a test can later
 *  add a tool and call `sendToolListChanged()`. */
export function installFakeMcpServer(id: string, spec: FakeMcpSpec): McpServer {
  const server = new McpServer({ name: `fake-${id}`, version: "0.0.0" });
  for (const t of spec.tools ?? []) {
    server.registerTool(t.name, { description: t.description ?? "", inputSchema: {} }, () => ({
      content: [{ type: "text", text: `${t.name} ok` }],
    }));
  }
  for (const r of spec.resources ?? []) {
    server.registerResource(r.name, r.uri, { mimeType: r.mimeType }, () => ({
      contents: [{ uri: r.uri, text: r.text ?? "", mimeType: r.mimeType }],
    }));
  }
  for (const p of spec.prompts ?? []) {
    server.registerPrompt(
      p.name,
      { description: p.description ?? "", argsSchema: p.requiredArg ? { topic: z.string() } : {} },
      () => ({ messages: [{ role: "user", content: { type: "text", text: p.text ?? "" } }] }),
    );
  }
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  void server.connect(serverTransport);
  fakes.set(id, { server, serverTransport, clientTransport });
  mcpManager().setTransportFactory((s) => {
    const fake = fakes.get(s.id);
    if (!fake) return Promise.reject(new Error(`no fake MCP server for ${s.id}`));
    return Promise.resolve(fake.clientTransport);
  });
  return server;
}

/** Add a tool to an already-installed fake and notify the client, exercising
 *  the tools/list_changed path. */
export function addToolAndNotify(id: string, name: string): void {
  const fake = fakes.get(id);
  if (!fake) throw new Error(`no fake MCP server for ${id}`);
  fake.server.registerTool(name, { description: "", inputSchema: {} }, () => ({
    content: [{ type: "text", text: `${name} ok` }],
  }));
  fake.server.sendToolListChanged();
}

/** Force a server-side close so the manager observes a dropped connection. */
export async function dropFakeMcpServer(id: string): Promise<void> {
  const fake = fakes.get(id);
  if (!fake) return;
  await fake.server.close();
}

/** Close any still-open fake servers. Call in a test's finally before teardown.
 *  The manager's own reset closes the client side. */
export async function clearFakeMcpServers(): Promise<void> {
  for (const fake of fakes.values()) await fake.server.close().catch(() => {});
  fakes.clear();
}
