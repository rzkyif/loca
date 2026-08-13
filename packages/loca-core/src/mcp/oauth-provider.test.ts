// OAuth provider vault round-trip + the connect-time guard: an oauth server that
// hasn't been signed into surfaces a clear "sign in required" status and makes no
// network call.

import { assert, assertEquals } from "@std/assert";
import { McpOAuthProvider } from "./oauth-provider.ts";
import { mcpManager } from "./manager.ts";
import { mcpRegistry } from "./registry.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";

Deno.test("provider persists client info + tokens + verifier through the vault", async () => {
  const env = await setupTestEnv();
  try {
    const provider = new McpOAuthProvider("srv1", "http://127.0.0.1:5000/callback");
    assertEquals(await provider.isAuthorized(), false);

    await provider.saveClientInformation({ client_id: "abc", redirect_uris: [] });
    await provider.saveCodeVerifier("verifier-123");
    await provider.saveTokens({ access_token: "tok", token_type: "Bearer" });

    // A fresh instance reads the same bag back from the vault.
    const reread = new McpOAuthProvider("srv1", "http://127.0.0.1:5000/callback");
    assertEquals((await reread.clientInformation())?.client_id, "abc");
    assertEquals(await reread.codeVerifier(), "verifier-123");
    assertEquals((await reread.tokens())?.access_token, "tok");
    assertEquals(await reread.isAuthorized(), true);

    // The metadata advertises the loopback redirect and a public (PKCE) client.
    assertEquals(provider.clientMetadata.redirect_uris, ["http://127.0.0.1:5000/callback"]);
    assertEquals(provider.clientMetadata.token_endpoint_auth_method, "none");
  } finally {
    await env.teardown();
  }
});

Deno.test("an unauthorized oauth server connects to 'sign in required', no network", async () => {
  const env = await setupTestEnv();
  try {
    const s = mcpRegistry().create({
      name: "OAuth",
      kind: "remote",
      url: "https://example.com/mcp",
      remoteAuth: "oauth",
      enabled: true,
    });
    await mcpManager().sync(mcpRegistry().list());
    const status = mcpManager().status(s.id);
    assertEquals(status.status, "error");
    assert(status.error?.includes("sign-in required"), status.error);
  } finally {
    await env.teardown();
  }
});
