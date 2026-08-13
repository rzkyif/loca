import type { CoreClient } from "./client";

export class CoreSettingsApi {
  constructor(private readonly client: CoreClient) {}

  /** GET /api/v1/settings → sparse record of non-default values. */
  load(): Promise<Record<string, unknown>> {
    return this.client.get("/api/v1/settings");
  }

  /** PATCH /api/v1/settings with the merged delta. Returns the full merged
   *  settings record. Pass null/undefined as a value to delete a key. */
  patch(partial: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.client.patch("/api/v1/settings", partial);
  }

  // --- secrets -----------------------------------------------------------
  // Stored encrypted on disk (secrets.enc, sealed via OS keychain). The
  // client only ever sees the NAME list, never the values.

  /** GET /api/v1/settings/secrets → { names: string[] } of populated secrets. */
  async listSecrets(): Promise<string[]> {
    const res = await this.client.get<{ names: string[] }>("/api/v1/settings/secrets");
    return res.names;
  }

  /** PUT /api/v1/settings/secrets/:name { value }. Empty `value` deletes. */
  async setSecret(name: string, value: string): Promise<void> {
    if (value === "") {
      await this.deleteSecret(name);
      return;
    }
    await this.client.put(`/api/v1/settings/secrets/${encodeURIComponent(name)}`, { value });
  }

  /** DELETE /api/v1/settings/secrets/:name. No-op if not present. */
  async deleteSecret(name: string): Promise<void> {
    await this.client.delete(`/api/v1/settings/secrets/${encodeURIComponent(name)}`);
  }
}
