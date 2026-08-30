/** Strip markdown out of LLM output before it reaches the TTS engine, so the
 *  voice does not read asterisks, backticks, URLs or code aloud. Safe on
 *  partial, mid-stream text. */
export function stripMarkdownForTTS(input: string): string {
  let text = input;

  // Fenced code blocks become " . " so the segmenter keeps the sentence
  // boundaries either side. A mid-stream unterminated fence truncates from its
  // opener so partial code never leaks; only a fence at the START of a line
  // opens a block, so a stray inline ``` cannot truncate the rest.
  text = text.replace(/```[\s\S]*?```/g, " . ");
  text = text.replace(/~~~[\s\S]*?~~~/g, " . ");
  const unterminatedFence = text.search(/^[ \t]*(?:```|~~~)/m);
  if (unterminatedFence !== -1) {
    text = text.slice(0, unterminatedFence);
  }

  // Un-escape before emphasis runs, or emphasis mis-matches the `\*` pair and
  // leaves dangling backslashes.
  text = text.replace(/\\([*_~`[\]()#>\-\\])/g, "$1");

  // Reference-style link/image definitions: `[id]: https://... "title"`.
  text = text.replace(/^[ \t]*\[[^\]]+\]:\s*\S.*$/gm, "");

  // Images: keep the alt text.
  text = text.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");

  // Links: keep the label; drop `<scheme://...>` autolinks.
  text = text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  text = text.replace(/<([a-z]+:\/\/[^>]+)>/gi, "");

  // Bare URLs: unreadable when voiced.
  text = text.replace(/https?:\/\/\S+/g, "");

  // HTML tags.
  text = text.replace(/<[^>]+>/g, "");

  // Horizontal rules.
  text = text.replace(/^[ \t]*[-*_]{3,}[ \t]*$/gm, "");

  // Headings: drop the hashes, keep the text.
  text = text.replace(/^[ \t]*#{1,6}[ \t]+/gm, "");

  // Blockquote markers.
  text = text.replace(/^[ \t]*>+[ \t]?/gm, "");

  // List markers.
  text = text.replace(/^[ \t]*(?:[-*+]|\d+\.)[ \t]+/gm, "");

  // Table separator rows.
  text = text.replace(/^[ \t]*\|?[ \t]*:?-{3,}[-:| \t]*$/gm, "");

  // Table pipes become commas so cells read as a listing.
  text = text.replace(/[ \t]*\|[ \t]*/g, ", ");

  // Emphasis: bold before italic so `**x**` isn't partially eaten; the
  // single-char variants use lookarounds to preserve `snake_case`.
  text = text.replace(/\*\*([^*\n]+?)\*\*/g, "$1");
  text = text.replace(/__([^_\n]+?)__/g, "$1");
  text = text.replace(/(?<![\w*])\*([^*\n]+?)\*(?!\w)/g, "$1");
  text = text.replace(/(?<![\w_])_([^_\n]+?)_(?!\w)/g, "$1");
  text = text.replace(/~~([^~\n]+?)~~/g, "$1");

  // Inline code.
  text = text.replace(/`([^`\n]+?)`/g, "$1");

  // Promote line breaks into sentence boundaries: without this the `\s+`
  // collapse below merges list items and headings into one run-on run that
  // Intl.Segmenter has no `.!?` to split on. `:` and `;` are excluded so
  // soft-pause endings are not double-punctuated.
  text = text.replace(/([^.!?:;\s])(\n+)/g, "$1.$2");

  // Pipe-substitution artifacts, then whitespace.
  text = text.replace(/(?:,\s*){2,}/g, ", ");
  text = text.replace(/^[\s,]+|[\s,]+$/g, "");
  text = text.replace(/\s+/g, " ");

  return text.trim();
}

// A base pictographic glyph plus trailing variation selectors and ZWJ-joined
// pictographs, so combined emoji leave no dangling joiners behind.
const EMOJI_PATTERN = /\p{Extended_Pictographic}(?:\u{FE0F}|\u{200D}\p{Extended_Pictographic})*/gu;

/** Strip emoji so the voice model does not try to pronounce them. Safe on
 *  partial, mid-stream text. */
export function stripEmojisForTTS(input: string): string {
  return input.replace(EMOJI_PATTERN, "").replace(/\s+/g, " ").trim();
}
