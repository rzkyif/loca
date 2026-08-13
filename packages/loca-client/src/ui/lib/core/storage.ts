import type { DeleteStoragePathsRequest, GetStorageResponse } from "@loca/shared";
import type { CoreClient } from "./client";

/** Storage view (Settings → Usage → Storage): the full on-disk tree across
 *  categories (models, binaries, sessions, extensions, cache, logs, settings) and
 *  the clear/delete operations. The core owns the files and is the authority on
 *  what's in use; this is a thin wrapper over its /storage endpoints. */
export class StorageApi {
  constructor(private readonly client: CoreClient) {}

  /** The full on-disk storage tree (every category + sizes + lock reasons). */
  get(): Promise<GetStorageResponse> {
    return this.client.get<GetStorageResponse>("/api/v1/storage");
  }

  /** Delete the selected files/folders. The core refuses anything in use. */
  deletePaths(paths: string[]): Promise<void> {
    const body: DeleteStoragePathsRequest = { paths };
    return this.client.post<void>("/api/v1/storage/delete", body);
  }

  /** Clear a whole category (its non-locked items). For "settings" this is a
   *  factory reset (defaults + wiped secrets). */
  clearCategory(categoryId: string): Promise<void> {
    return this.client.post<void>("/api/v1/storage/clear", { categoryId });
  }
}
