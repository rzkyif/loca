/**
 * Defines the snippet shape and the logic that expands trigger tokens inside
 * the user's input. A snippet fires on one of three symbols - `#`, `@`, or `/` -
 * each with its own autocomplete list in the message box. The symbol is the
 * user's free choice, but a recommended one is derived from the placement and
 * set automatically on create and when the placement changes (until the user
 * picks their own). Expansion walks the raw text, substitutes matching
 * snippets, and returns the final text plus any system-prompt overrides.
 */

export type SnippetSymbol = "#" | "@" | "/";

export const SNIPPET_SYMBOLS: SnippetSymbol[] = ["#", "@", "/"];

export type SnippetPlacement =
  | "prepend-system"
  | "replace-system"
  | "append-system"
  | "prepend-user"
  | "replace-user"
  | "insert-user"
  | "append-user";

export type Snippet = {
  id: string;
  name: string; // bare name, no symbol; [A-Za-z0-9_-]
  symbol: SnippetSymbol;
  // True once the user picked the symbol themselves, so a later placement
  // change must not auto-retrack the recommendation. Inferred from a mismatch
  // with the recommendation for hand-written files that predate the bit.
  symbolPinned: boolean;
  placement: SnippetPlacement;
  text: string;
};

export type SnippetOverride = {
  prepend?: string;
  replace?: string;
  append?: string;
};

export type SnippetApplyResult = {
  userText: string;
  systemOverride?: SnippetOverride;
};

export const SNIPPET_PLACEMENT_OPTIONS: {
  value: SnippetPlacement;
  label: string;
}[] = [
  { value: "prepend-system", label: "Prepend System Prompt" },
  { value: "replace-system", label: "Replace System Prompt" },
  { value: "append-system", label: "Append System Prompt" },
  { value: "prepend-user", label: "Prepend User Prompt" },
  { value: "replace-user", label: "Replace User Prompt" },
  { value: "insert-user", label: "Insert in User Prompt" },
  { value: "append-user", label: "Append User Prompt" },
];

// The trigger symbol that best fits a placement, marked "(Recommended)" in the
// editor and applied automatically: `#` inserts inline, `/` acts like a command
// over the whole user prompt, `@` references context fed to the system prompt.
export function recommendedSymbol(placement: SnippetPlacement): SnippetSymbol {
  if (placement === "insert-user") return "#";
  if (placement.endsWith("-system")) return "@";
  return "/";
}

/** The full display trigger for a snippet, e.g. `/summarize`. */
export function snippetTrigger(s: Pick<Snippet, "symbol" | "name">): string {
  return `${s.symbol}${s.name}`;
}

// Matches a `#`/`@`/`/` token only when preceded by start-of-string or a
// boundary char that is neither a word char nor another trigger symbol. Stops
// `email@domain` and `http://x` from being misread mid-token. A name is any
// non-whitespace run (minus a trailing sentence-punctuation mark) so a memory
// reference like `@ext/skills/foo` is one token and a snippet inside it can't be
// mis-expanded; a snippet's own name stays bare `[A-Za-z0-9_-]` (validateName).
const TRIGGER_SCAN = /(^|[^\w@#/])([#@/][^\s"]*[^\s".,:;!?)\]}'])/g;

// Autocomplete text-before-caret scan. Captures the symbol plus the partial
// name being typed: a bare partial (any non-whitespace, so a dotted/slashed
// memory name keeps the dropdown open as it is typed) or an open quoted
// `"partial name` (unterminated). The quoted form is only used by `@` references
// (memories / resources) whose names may contain spaces; snippet names are
// always bare.
export const TRIGGER_BEFORE_CARET = /(?:^|[^\w@#/])([#@/](?:"[^"]*|[^\s"]*))$/;

/** Strip any leading symbol(s) and whitespace from raw input to get a bare
 *  snippet name. */
export function normalizeName(raw: string): string {
  return raw.replace(/^[#@/]+/, "").replace(/\s+/g, "");
}

export function validateName(
  symbol: SnippetSymbol,
  name: string,
  otherTriggers: string[],
): string | null {
  if (!name) return "Name is required";
  if (!/^[A-Za-z0-9_-]+$/.test(name)) {
    return "Only letters, numbers, underscore, and dash are allowed";
  }
  if (otherTriggers.includes(`${symbol}${name}`.toLowerCase())) {
    return "This trigger is already used by another snippet";
  }
  return null;
}

/**
 * Walk the raw user text left-to-right, expanding snippet triggers according
 * to their placement. Tokens that don't resolve to any known snippet are left
 * in place (so a stray `@foo` memory reference still reaches the core, and a
 * stray `/bar` survives untouched).
 *
 * Composition rules:
 *  - prepend-user / append-user / insert-user modify the user text in place.
 *  - prepend-system / append-system accumulate into the override, joined by
 *    "\n\n" in trigger-appearance order.
 *  - replace-user and replace-system are last-wins.
 */
export function applySnippets(raw: string, snippets: Snippet[]): SnippetApplyResult {
  if (!raw || snippets.length === 0) {
    return { userText: raw };
  }

  const byTrigger = new Map<string, Snippet>();
  for (const s of snippets) {
    if (s.name) byTrigger.set(snippetTrigger(s).toLowerCase(), s);
  }
  if (byTrigger.size === 0) return { userText: raw };

  const prependUserParts: string[] = [];
  const appendUserParts: string[] = [];
  let replaceUserValue: string | null = null;
  let anyReplaceUser = false;

  const prependSystemParts: string[] = [];
  const appendSystemParts: string[] = [];
  let replaceSystemValue: string | null = null;
  let hasSystemOverride = false;

  let inlinedText = "";
  let cursor = 0;
  TRIGGER_SCAN.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = TRIGGER_SCAN.exec(raw)) !== null) {
    const leading = match[1];
    const token = match[2];
    const snippet = byTrigger.get(token.toLowerCase());

    // Everything up to and including the boundary char is kept verbatim.
    const boundaryEnd = match.index + leading.length;
    inlinedText += raw.slice(cursor, boundaryEnd);

    if (!snippet) {
      // Unknown trigger: leave the token as-is.
      inlinedText += token;
      cursor = boundaryEnd + token.length;
      continue;
    }

    switch (snippet.placement) {
      case "insert-user":
        inlinedText += snippet.text;
        break;
      case "prepend-user":
        prependUserParts.push(snippet.text);
        break;
      case "append-user":
        appendUserParts.push(snippet.text);
        break;
      case "replace-user":
        anyReplaceUser = true;
        replaceUserValue = snippet.text;
        break;
      case "prepend-system":
        prependSystemParts.push(snippet.text);
        hasSystemOverride = true;
        break;
      case "append-system":
        appendSystemParts.push(snippet.text);
        hasSystemOverride = true;
        break;
      case "replace-system":
        replaceSystemValue = snippet.text;
        hasSystemOverride = true;
        break;
    }

    // A non-inline placement removes its token from the body and contributes
    // nothing in its place, so the boundary whitespace that preceded it (e.g.
    // the space in "hello @sys") is a leftover artifact: drop it. `insert-user`
    // keeps its surroundings since it substitutes text inline.
    if (snippet.placement !== "insert-user") {
      inlinedText = inlinedText.replace(/\s+$/, "");
    }

    cursor = boundaryEnd + token.length;
  }
  inlinedText += raw.slice(cursor);

  let userText: string;
  if (anyReplaceUser) {
    userText = replaceUserValue ?? "";
  } else {
    // Order: prepended snippets, the user's body (token-expanded), appended
    // snippets. Drop empty/whitespace-only segments, then join with a blank
    // line. A single segment is returned verbatim so the user's own body keeps
    // its leading indentation and trailing whitespace; only when an affix sits
    // next to the body do we trim the whitespace FACING that seam, so the parts
    // join cleanly without swallowing the body's formatting.
    const segments = [
      prependUserParts.length ? prependUserParts.join("\n\n") : "",
      inlinedText,
      appendUserParts.length ? appendUserParts.join("\n\n") : "",
    ].filter((s) => s.trim() !== "");
    if (segments.length <= 1) {
      userText = segments[0] ?? "";
    } else {
      userText = segments
        .map((s, i) => {
          if (i === 0) return s.replace(/\s+$/, "");
          if (i === segments.length - 1) return s.replace(/^\s+/, "");
          return s.trim();
        })
        .join("\n\n");
    }
  }

  const systemOverride: SnippetOverride | undefined = hasSystemOverride
    ? {
        ...(prependSystemParts.length ? { prepend: prependSystemParts.join("\n\n") } : {}),
        ...(replaceSystemValue !== null ? { replace: replaceSystemValue } : {}),
        ...(appendSystemParts.length ? { append: appendSystemParts.join("\n\n") } : {}),
      }
    : undefined;

  return { userText, systemOverride };
}
