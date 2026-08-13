// Tiny shared fixtures the sample tools reuse: a 1x1 transparent PNG so the
// image and display samples have a real (if minimal) image to show without
// bundling any asset.

export const SAMPLE_PNG_B64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

export const SAMPLE_PNG_MIME = "image/png";

/** Read a string arg, returning a fallback when absent or wrong-typed. */
export function stringArg(args: Record<string, unknown>, key: string, fallback = ""): string {
  const value = args[key];
  return typeof value === "string" ? value : fallback;
}

/** Read a positive-integer arg, returning a fallback when absent or invalid. */
export function intArg(args: Record<string, unknown>, key: string, fallback: number): number {
  const value = args[key];
  return typeof value === "number" && Number.isFinite(value) ? Math.trunc(value) : fallback;
}
