import type { UpdateCheckResponse } from "@loca/shared";
import type { CoreClient } from "./client";

export class UpdateApi {
  constructor(private readonly client: CoreClient) {}

  /** Fetches and parses the signed core manifest, comparing its version
   *  against the running core. Cheap (just a manifest fetch + signature
   *  verify), so it's safe to call on a "Check for Updates" button click. */
  check(): Promise<UpdateCheckResponse> {
    return this.client.get("/api/v1/update/check");
  }

  /** Downloads the staged binary, hands off to loca-core-updater, and
   *  exits the core. The HTTP request will not return cleanly in the
   *  success case: the core exits before flushing the 204. Callers
   *  should swallow request-aborted errors and watch the
   *  `update.staged` WS frame instead. */
  apply(version?: string): Promise<void> {
    return this.client.post("/api/v1/update/apply", { version });
  }
}
