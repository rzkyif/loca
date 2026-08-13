// Report the current date and time in the host's local timezone. Needs no
// permissions and is alwaysAvailable so the model can anchor relative dates
// ("tomorrow", "next friday") instead of guessing.

import type { ToolContext } from "./types.ts";

export function getDatetime(
  _args: Record<string, unknown>,
  _ctx: ToolContext,
): {
  iso: string;
  date: string;
  time: string;
  weekday: string;
  timezone: string;
  utcOffsetMinutes: number;
  epochMs: number;
} {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  const time = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  const weekday = now.toLocaleDateString("en-US", { weekday: "long" });
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone ?? "";
  // getTimezoneOffset is minutes *behind* UTC; flip so east-of-UTC is positive.
  const utcOffsetMinutes = -now.getTimezoneOffset();
  const sign = utcOffsetMinutes < 0 ? "-" : "+";
  const abs = Math.abs(utcOffsetMinutes);
  const iso = `${date}T${time}${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
  return {
    iso,
    date,
    time,
    weekday,
    timezone,
    utcOffsetMinutes,
    epochMs: now.getTime(),
  };
}
