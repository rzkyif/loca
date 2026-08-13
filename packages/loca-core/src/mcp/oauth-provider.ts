// Vault-backed OAuthClientProvider for a remote MCP server. Persists the dynamic
// client registration, the issued tokens, and the in-flight PKCE verifier as a
// single JSON bag in the secrets vault (keyed by server id), so the SDK can run
// and refresh the OAuth 2.1 authorization-code flow. The redirect target is a
// loopback URL the sign-in flow opens a listener on (RFC 8252 native-app flow);
// `redirectToAuthorization` captures the authorization URL for the flow to hand
// to the client to open in a browser.

import type { OAuthClientProvider } from "@modelcontextprotocol/sdk/client/auth.js";
import type {
  OAuthClientInformationFull,
  OAuthClientMetadata,
  OAuthTokens,
} from "@modelcontextprotocol/sdk/shared/auth.js";
import { getSecret, setSecret } from "@loca/core-engine/services/secrets";
import { mcpOAuthSecretName } from "./secret-key.ts";

interface OAuthBag {
  clientInformation?: OAuthClientInformationFull;
  tokens?: OAuthTokens;
  codeVerifier?: string;
}

export class McpOAuthProvider implements OAuthClientProvider {
  private bag: OAuthBag | null = null;
  /** The authorization URL captured on the last `redirectToAuthorization`. */
  authorizationUrl?: URL;

  constructor(
    private serverId: string,
    private loopbackRedirect: string,
  ) {}

  get redirectUrl(): string {
    return this.loopbackRedirect;
  }

  get clientMetadata(): OAuthClientMetadata {
    return {
      client_name: "Loca",
      redirect_uris: [this.loopbackRedirect],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      // Public client: no client secret, security comes from PKCE.
      token_endpoint_auth_method: "none",
    };
  }

  async clientInformation(): Promise<OAuthClientInformationFull | undefined> {
    return (await this.load()).clientInformation;
  }

  async saveClientInformation(info: OAuthClientInformationFull): Promise<void> {
    await this.patch({ clientInformation: info });
  }

  async tokens(): Promise<OAuthTokens | undefined> {
    return (await this.load()).tokens;
  }

  async saveTokens(tokens: OAuthTokens): Promise<void> {
    await this.patch({ tokens });
  }

  redirectToAuthorization(authorizationUrl: URL): void {
    this.authorizationUrl = authorizationUrl;
  }

  async saveCodeVerifier(codeVerifier: string): Promise<void> {
    await this.patch({ codeVerifier });
  }

  async codeVerifier(): Promise<string> {
    const v = (await this.load()).codeVerifier;
    if (!v) throw new Error("no PKCE code verifier saved for this MCP server");
    return v;
  }

  /** Whether the flow has completed (tokens are stored). */
  async isAuthorized(): Promise<boolean> {
    return (await this.load()).tokens !== undefined;
  }

  private async load(): Promise<OAuthBag> {
    if (this.bag) return this.bag;
    const raw = await getSecret(mcpOAuthSecretName(this.serverId));
    this.bag = raw ? (JSON.parse(raw) as OAuthBag) : {};
    return this.bag;
  }

  private async patch(delta: Partial<OAuthBag>): Promise<void> {
    const bag = { ...(await this.load()), ...delta };
    this.bag = bag;
    await setSecret(mcpOAuthSecretName(this.serverId), JSON.stringify(bag));
  }
}
