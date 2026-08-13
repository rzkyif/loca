// Fetch a web page and extract its readable content: Mozilla Readability
// over a linkedom DOM, falling back to the body text when the page has no
// article. Returns plain text, capped so one page can't flood the model's
// context.

import { Readability } from "@mozilla/readability";
import { DOMParser } from "linkedom";
import type { ToolContext } from "./types.ts";
import { BROWSER_UA, fetchTextCapped } from "./web.ts";

const CONTENT_MAX_CHARS = 40_000;

export async function fetchWebpage(
  args: { url?: string },
  ctx: ToolContext,
): Promise<{ url: string; title: string; content: string; truncated: boolean }> {
  const url = typeof args?.url === "string" ? args.url.trim() : "";
  if (!/^https?:\/\//i.test(url)) {
    throw new Error("only http(s) URLs are allowed");
  }

  ctx.setProgress(0.2, "Fetching", url);
  const { text, finalUrl, contentType } = await fetchTextCapped(url, {
    signal: ctx.signal,
    headers: {
      "user-agent": BROWSER_UA,
      accept: "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8",
    },
  });

  // Non-HTML responses (plain text, markdown, JSON APIs) pass through as-is.
  if (contentType && !contentType.includes("html")) {
    const content = text.slice(0, CONTENT_MAX_CHARS);
    return {
      url: finalUrl,
      title: "",
      content,
      truncated: content.length < text.length,
    };
  }

  ctx.setProgress(0.7, "Extracting content", finalUrl);
  const document = new DOMParser().parseFromString(text, "text/html");
  const article = new Readability(document).parse();
  const title = (article?.title ?? document.title ?? "").trim();
  const raw = normalizeText(article?.textContent || document.body?.textContent || "");
  const content = raw.slice(0, CONTENT_MAX_CHARS);
  return {
    url: finalUrl,
    title,
    content,
    truncated: content.length < raw.length,
  };
}

/** Whole-page textContent is riddled with indentation and blank-line runs;
 *  collapse them so the capped budget is spent on words. */
function normalizeText(s: string): string {
  return s
    .replace(/[ \t]+/g, " ")
    .replace(/ ?\n ?/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
