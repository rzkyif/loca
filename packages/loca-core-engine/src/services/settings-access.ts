/**
 * Tiny typed readers for the sparse core settings record. A key absent from the
 * record (or stored with the wrong type) falls back to `def`. `numSetting` also
 * coerces a non-empty numeric string, since hand-edited settings.json values
 * and some form inputs arrive as strings.
 */

export function strSetting(s: Record<string, unknown>, key: string, def: string): string {
  const v = s[key];
  return typeof v === "string" ? v : def;
}

export function numSetting(s: Record<string, unknown>, key: string, def: number): number {
  const v = s[key];
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v !== "") {
    const n = Number(v);
    if (Number.isFinite(n)) return n;
  }
  return def;
}

export function boolSetting(s: Record<string, unknown>, key: string, def: boolean): boolean {
  const v = s[key];
  return typeof v === "boolean" ? v : def;
}
