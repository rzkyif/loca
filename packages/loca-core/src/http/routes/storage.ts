import { Hono } from "hono";
import type { DeleteStoragePathsRequest } from "@loca/shared";
import {
  buildStorageTree,
  clearStorageCategory,
  deleteStoragePaths,
} from "../../services/storage.ts";
import { AppError } from "@loca/core-engine";
import { readJson } from "@loca/core-engine/http/body";
import { bearerMiddleware } from "../middleware/auth.ts";

export function storageRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  // The full on-disk storage tree (every category + sizes) for the Settings
  // view.
  r.get("/", async (c) => c.json(await buildStorageTree()));

  // Delete the selected files/folders. The core re-derives each item's lock
  // server-side and refuses anything in use; unknown paths are skipped.
  // Recomputes requirements so a now-missing model re-surfaces.
  r.post("/delete", async (c) => {
    const body = (await readJson(c)) as DeleteStoragePathsRequest;
    if (!Array.isArray(body.paths)) {
      throw new AppError("validation_error", "paths array required");
    }
    await deleteStoragePaths(body.paths);
    return c.body(null, 204);
  });

  // Clear an entire category: delete its non-locked items. For `settings` this
  // is a factory reset (defaults + wiped secrets).
  r.post("/clear", async (c) => {
    const body = (await readJson(c)) as { categoryId?: unknown };
    if (typeof body.categoryId !== "string") {
      throw new AppError("validation_error", "categoryId string required");
    }
    await clearStorageCategory(body.categoryId);
    return c.body(null, 204);
  });

  return r;
}
