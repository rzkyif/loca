import type {
  BinaryKind,
  BinaryManifest,
  CheckBinariesResponse,
  InstallBinariesResponse,
  ListBinariesResponse,
  ProbeBinariesResponse,
  UpdateBinaryResponse,
} from "@loca/shared";
import type { CoreClient } from "./client";

export class BinariesApi {
  constructor(private readonly client: CoreClient) {}

  list(): Promise<ListBinariesResponse> {
    return this.client.get("/api/v1/binaries");
  }

  install(kinds?: BinaryKind[]): Promise<InstallBinariesResponse> {
    return this.client.post("/api/v1/binaries/install", { kinds });
  }

  update(kind: BinaryKind): Promise<UpdateBinaryResponse> {
    return this.client.post("/api/v1/binaries/update", { kind });
  }

  manifest(): Promise<BinaryManifest> {
    return this.client.get("/api/v1/binaries/manifest");
  }

  check(): Promise<CheckBinariesResponse> {
    return this.client.get("/api/v1/binaries/check");
  }

  probe(kinds: BinaryKind[]): Promise<ProbeBinariesResponse> {
    return this.client.post("/api/v1/binaries/probe", { kinds });
  }
}
