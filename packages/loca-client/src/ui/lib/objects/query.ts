/**
 * VS Code-style query token helpers for the object-management search bar.
 *
 * A query is whitespace-delimited words. A word beginning with `@` is a token:
 * `@sort:<value>` sets the sort; any other `@<id>` is a filter. Everything else
 * is free text. Clicking a filter/sort option in the menu inserts or toggles
 * its token in place, keeping it to the right of the last existing token and to
 * the left of the free text (e.g. `@installed cat` + sort:name ->
 * `@installed @sort:name cat`). Pure functions, no Svelte; unit-tested in
 * query.test.ts.
 */

/** A parsed query: `text` is the free-text remainder; `filters` is the set of
 *  active bare `@<id>` filter tokens (without the leading @); `sort` is the
 *  value of the single `@sort:<x>` token if present. */
export interface ParsedQuery {
  text: string;
  filters: Set<string>;
  sort: string | null;
}

const SORT_PREFIX = "@sort:";

type Kind = "sort" | "filter" | "text";

function classify(word: string): Kind {
  if (word.startsWith(SORT_PREFIX)) return "sort";
  if (word.length > 1 && word.startsWith("@")) return "filter";
  return "text";
}

/** Split a raw query into non-empty whitespace-delimited words. */
function words(raw: string): string[] {
  return raw.split(/\s+/).filter((w) => w.length > 0);
}

/** Parse a raw query string into structured parts. The last `@sort:` wins; a
 *  bare `@sort:` (no value) yields a null sort. */
export function parseQuery(raw: string): ParsedQuery {
  const filters = new Set<string>();
  let sort: string | null = null;
  const text: string[] = [];
  for (const w of words(raw)) {
    switch (classify(w)) {
      case "sort": {
        const v = w.slice(SORT_PREFIX.length);
        sort = v.length > 0 ? v : null;
        break;
      }
      case "filter":
        filters.add(w.slice(1));
        break;
      case "text":
        text.push(w);
        break;
    }
  }
  return { text: text.join(" "), filters, sort };
}

/** Insert `token` immediately after the last @-token and before the first text
 *  word. Mutates and returns `tokens`. */
function insertAfterTokens(tokens: string[], token: string): string[] {
  let lastTokenIdx = -1;
  for (let i = 0; i < tokens.length; i++) {
    if (classify(tokens[i]) !== "text") lastTokenIdx = i;
  }
  tokens.splice(lastTokenIdx + 1, 0, token);
  return tokens;
}

/** Toggle a bare filter token (e.g. "installed") in the raw query, preserving
 *  the "right of the last @-token, left of free text" placement on insert. */
export function toggleFilterToken(raw: string, token: string): string {
  const target = `@${token}`;
  const tokens = words(raw);
  const present = tokens.includes(target);
  const next = present ? tokens.filter((w) => w !== target) : insertAfterTokens(tokens, target);
  return next.join(" ");
}

/** Insert or REPLACE the single `@sort:<value>` token; re-selecting the active
 *  sort clears it. */
export function setSortToken(raw: string, value: string): string {
  const current = parseQuery(raw).sort;
  const withoutSort = words(raw).filter((w) => classify(w) !== "sort");
  if (current === value) return withoutSort.join(" ");
  return insertAfterTokens(withoutSort, `${SORT_PREFIX}${value}`).join(" ");
}
