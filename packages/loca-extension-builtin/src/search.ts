// Keyless web search against DuckDuckGo's HTML endpoint, parsed with
// linkedom into {title, url, snippet} results. Result links are DDG
// redirect URLs; the real target is recovered from their `uddg` param.

import { DOMParser } from "linkedom";
import type { ToolContext } from "./types.ts";
import { BROWSER_UA, fetchTextCapped } from "./web.ts";

const MAX_RESULTS = 10;

export async function webSearch(
  args: { query?: string; maxResults?: number },
  ctx: ToolContext,
): Promise<{
  query: string;
  results: Array<{ title: string; url: string; snippet: string }>;
}> {
  const query = typeof args?.query === "string" ? args.query.trim() : "";
  if (!query) throw new Error("query is required");
  const rawMax = typeof args?.maxResults === "number" ? Math.trunc(args.maxResults) : 5;
  const wanted = Math.min(Math.max(rawMax, 1), MAX_RESULTS);

  ctx.setProgress(0.3, "Searching", query);
  const { text: html } = await fetchTextCapped(
    `https://html.duckduckgo.com/html/?q=${encodeURIComponent(query)}`,
    { signal: ctx.signal, headers: { "user-agent": BROWSER_UA } },
  );

  const document = new DOMParser().parseFromString(html, "text/html");
  const results: Array<{ title: string; url: string; snippet: string }> = [];
  for (const el of document.querySelectorAll(".result")) {
    const anchor = el.querySelector("a.result__a");
    const title = anchor?.textContent?.trim() ?? "";
    const url = resolveDdgUrl(anchor?.getAttribute("href") ?? "");
    if (!title || !url) continue;
    const snippet = el.querySelector(".result__snippet")?.textContent?.trim() ?? "";
    results.push({ title, url, snippet });
    if (results.length >= wanted) break;
  }

  if (results.length === 0 && /anomaly/i.test(html)) {
    throw new Error("DuckDuckGo is rate-limiting searches from this machine; try again later");
  }
  ctx.setProgress(1, "Searched", `${results.length} results`);
  return { query, results };
}

/** DDG result hrefs are redirects like
 *  `//duckduckgo.com/l/?uddg=<encoded target>&rut=...`; unwrap to the real
 *  target and drop anything that is not http(s). */
function resolveDdgUrl(href: string): string {
  if (!href) return "";
  try {
    const u = new URL(href, "https://html.duckduckgo.com");
    const uddg = u.searchParams.get("uddg");
    const target = uddg ?? u.href;
    return /^https?:\/\//i.test(target) ? target : "";
  } catch {
    return "";
  }
}
