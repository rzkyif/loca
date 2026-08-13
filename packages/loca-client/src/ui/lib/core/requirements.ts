import type { DownloadRequirementsResponse, GetRequirementsResponse } from "@loca/shared";
import type { CoreClient } from "./client";

export class RequirementsApi {
  constructor(private readonly client: CoreClient) {}

  /** The full required-files list + the missing subset for the current config.
   *  `probe:true` fills size/version hints via outbound HEADs and must be sent
   *  ONLY from an explicit user action (opening the Pending Downloads modal),
   *  never on the always-on connect/settings refetch. */
  get(opts?: { probe?: boolean }): Promise<GetRequirementsResponse> {
    return this.client.get(`/api/v1/requirements${opts?.probe ? "?probe=1" : ""}`);
  }

  /** Download everything currently missing (models + binaries). */
  download(): Promise<DownloadRequirementsResponse> {
    return this.client.post("/api/v1/requirements/download", {});
  }
}
