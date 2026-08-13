import { Hono } from "hono";
import type { BinaryKind } from "@loca/shared";
import { binarySourceToKind, errMessage } from "@loca/shared";
import { computeRequirements } from "../../services/requirements.ts";
import { ensureKindModels, type ModelKind } from "../../services/model-ensure.ts";
import { binariesManager } from "../../binaries/manager.ts";
import { bearerMiddleware } from "../middleware/auth.ts";
import { getLogger } from "../../shared/log.ts";

const log = getLogger("requirements");

export function requirementsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  // The full required-files list + the missing subset for the current config.
  // `?probe=1` additionally fills size/version hints via outbound HEADs; it is
  // sent ONLY when the user opens the Pending Downloads confirm modal (an
  // explicit action), never on the always-on connect/settings snapshot, per the
  // no-non-consented-network rule.
  r.get("/", async (c) =>
    c.json(await computeRequirements({ probeSizes: c.req.query("probe") === "1" })),
  );

  // Download everything missing: enqueue each missing model group (reusing the
  // probe+enqueue+dedup in ensureKindModels) and install the missing binaries.
  // `missing` already excludes binaries unavailable on this platform.
  r.post("/download", async (c) => {
    const snap = await computeRequirements();

    const groups = new Set<ModelKind>(
      snap.missing.filter((m) => m.type === "model").map((m) => m.group as ModelKind),
    );
    const modelJobIds: string[] = [];
    for (const g of groups) {
      // One group failing to enqueue (e.g. a malformed source spec) must not
      // abort the whole request and starve the OTHER groups + the binary
      // installs below. Log and continue; the group stays missing and the popup
      // keeps offering it.
      try {
        const res = await ensureKindModels(g);
        modelJobIds.push(...res.enqueued);
      } catch (err) {
        log.error(`requirements download: ensure ${g} failed: ${errMessage(err)}`);
      }
    }

    const binKinds: BinaryKind[] = snap.missing
      .filter((m) => m.type === "binary")
      .map((m) => binarySourceToKind(m.source));
    const { jobIds: binaryJobIds } =
      binKinds.length > 0 ? await binariesManager().install(binKinds) : { jobIds: [] };

    return c.json({ modelJobIds, binaryJobIds });
  });

  return r;
}
