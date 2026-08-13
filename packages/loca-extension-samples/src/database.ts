// Database sample: a pure ctx.db demo distinct from the table tool. Upsert a
// key/value into a tiny private table, then read it back. Shows the
// per-extension SQLite that "database": true provisions.

import type { ToolContext } from "./types.ts";
import { stringArg } from "./sample-data.ts";

export async function sampleDatabase(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ key: string; value: string }> {
  const key = stringArg(args, "key", "greeting");
  const value = stringArg(args, "value", "hello");

  await ctx.db.execute("CREATE TABLE IF NOT EXISTS sample_kv (key TEXT PRIMARY KEY, value TEXT)");
  await ctx.db.execute(
    "INSERT INTO sample_kv (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    [key, value],
  );

  const rows = await ctx.db.query("SELECT key, value FROM sample_kv WHERE key = ?", [key]);
  const row = rows[0];
  return {
    key: typeof row?.key === "string" ? row.key : key,
    value: typeof row?.value === "string" ? row.value : value,
  };
}
