import { Hono } from "hono";
import type { BinaryKind } from "@loca/shared";
import { binariesManager } from "../../binaries/manager.ts";
import { loadBinaryManifest } from "../../binaries/manifest.ts";
import { AppError } from "@loca/core-engine";
import { readJson } from "@loca/core-engine/http/body";
import { bearerMiddleware } from "../middleware/auth.ts";

export function binariesRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/", async (c) => c.json(await binariesManager().list()));

  r.post("/install", async (c) => {
    const body = (await readJson(c, { allowEmpty: true })) as { kinds?: BinaryKind[] };
    const result = await binariesManager().install(body.kinds);
    return c.json(result);
  });

  r.post("/update", async (c) => {
    const body = (await readJson(c, { allowEmpty: true })) as { kind: BinaryKind };
    if (!body.kind) throw new AppError("validation_error", "kind required");
    return c.json(await binariesManager().update(body.kind));
  });

  r.get("/manifest", async (c) => c.json(await loadBinaryManifest()));

  r.get("/check", async (c) => c.json(await binariesManager().check()));

  r.post("/probe", async (c) => {
    const body = (await readJson(c, { allowEmpty: true })) as { kinds?: BinaryKind[] };
    return c.json(await binariesManager().probe(body.kinds ?? []));
  });

  return r;
}
