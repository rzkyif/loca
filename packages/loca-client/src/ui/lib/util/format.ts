/**
 * Shared formatting helpers for human-readable display values.
 */

import type { CatalogModelView, QuantOption } from "@loca/shared";

/** The model's display name on its own, for tight surfaces like the quick model
 *  controls. The full settings picker adds size range / fit suffixes; here we
 *  want just the name. */
export function shortModelName(model: CatalogModelView): string {
  return model.name;
}

/** The quant's id on its own (e.g. "Q4_K_M"), with the variant appended only
 *  when it isn't the plain "standard" one. Drops the size / recommended / fit
 *  detail the full picker shows. */
export function shortQuantName(quant: QuantOption): string {
  return quant.variantLabel && quant.variantLabel !== "standard"
    ? `${quant.quant} (${quant.variantLabel})`
    : quant.quant;
}

export function formatBytes(b: number | null | undefined): string {
  if (b == null) return "size unknown";
  if (b >= 1024 ** 3) return `${(b / 1024 ** 3).toFixed(2)} GB`;
  if (b >= 1024 ** 2) return `${(b / 1024 ** 2).toFixed(1)} MB`;
  if (b >= 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${b} B`;
}

/** The fallback title for a session that the user never named: its creation
 *  datetime, formatted `YYYY-MM-DD HH:MM`. Shown as the session-bar title
 *  placeholder and as the session-list label for untitled sessions. */
export function formatSessionDefaultTitle(createdAtMs: number): string {
  const d = new Date(createdAtMs);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}`;
}
