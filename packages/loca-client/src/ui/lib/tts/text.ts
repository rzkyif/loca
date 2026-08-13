/**
 * Strips markdown formatting out of LLM output before it goes to the
 * text-to-speech engine, so the voice doesn't end up reading asterisks,
 * backticks, URLs, or code-block contents aloud. Safe to run on partial,
 * mid-stream text.
 */
export function stripMarkdownForTTS(input: string): string {
  let text = input;

  // 1. Fenced code blocks. Replace with " . " so sentence boundaries on
  //    either side of the block are preserved for the segmenter. Handle
  //    unterminated fences (mid-stream) by truncating from the opener so we
  //    don't leak partial code content before the closing fence arrives.
  text = text.replace(/```[\s\S]*?```/g, " . ");
  text = text.replace(/~~~[\s\S]*?~~~/g, " . ");
  // Only a fence at the START of a line opens a code block; an inline stray
  // triple-backtick (e.g. "use ``` to fence code") must NOT truncate the rest
  // of the text, which the old un-anchored search did.
  const unterminatedFence = text.search(/^[ \t]*(?:```|~~~)/m);
  if (unterminatedFence !== -1) {
    text = text.slice(0, unterminatedFence);
  }

  // 2. Un-escape markdown punctuation early so `\*star\*` becomes `*star*`
  //    before emphasis runs - otherwise emphasis mis-matches the `\*` pair
  //    and leaves dangling backslashes in the output.
  text = text.replace(/\\([*_~`[\]()#>\-\\])/g, "$1");

  // 3. Reference-style link/image definitions: `[id]: https://... "title"`.
  text = text.replace(/^[ \t]*\[[^\]]+\]:\s*\S.*$/gm, "");

  // 3. Images `![alt](url)` - keep alt text if any.
  text = text.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");

  // 4. Links `[text](url)` - keep the label. Also drop `<scheme://...>` autolinks.
  text = text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  text = text.replace(/<([a-z]+:\/\/[^>]+)>/gi, "");

  // 5. Bare URLs in running prose - unreadable when voiced.
  text = text.replace(/https?:\/\/\S+/g, "");

  // 6. HTML tags.
  text = text.replace(/<[^>]+>/g, "");

  // 7. Horizontal rules.
  text = text.replace(/^[ \t]*[-*_]{3,}[ \t]*$/gm, "");

  // 8. Headings - drop the leading hashes, keep the text.
  text = text.replace(/^[ \t]*#{1,6}[ \t]+/gm, "");

  // 9. Blockquote markers.
  text = text.replace(/^[ \t]*>+[ \t]?/gm, "");

  // 10. List markers (bulleted and numbered).
  text = text.replace(/^[ \t]*(?:[-*+]|\d+\.)[ \t]+/gm, "");

  // 11. Table separator rows (| --- | :--- | ---: |).
  text = text.replace(/^[ \t]*\|?[ \t]*:?-{3,}[-:| \t]*$/gm, "");

  // 12. Table pipes: turn into commas so cells become a natural-language listing.
  text = text.replace(/[ \t]*\|[ \t]*/g, ", ");

  // 13. Inline emphasis / strikethrough. Bold before italic so `**x**` isn't
  //     partially eaten. Italic single-char variants use lookarounds to
  //     preserve `snake_case` identifiers.
  text = text.replace(/\*\*([^*\n]+?)\*\*/g, "$1");
  text = text.replace(/__([^_\n]+?)__/g, "$1");
  text = text.replace(/(?<![\w*])\*([^*\n]+?)\*(?!\w)/g, "$1");
  text = text.replace(/(?<![\w_])_([^_\n]+?)_(?!\w)/g, "$1");
  text = text.replace(/~~([^~\n]+?)~~/g, "$1");

  // 14. Inline code.
  text = text.replace(/`([^`\n]+?)`/g, "$1");

  // 15. Promote line breaks into sentence boundaries when the line above
  //     doesn't already end with terminal punctuation. Without this, the
  //     next step's `\s+` collapse merges list items, headings, and short
  //     paragraphs into one run-on "sentence" that Intl.Segmenter cannot
  //     split because it has no .!? to break on. `:` and `;` stay in the
  //     negated class so we don't double-punctuate soft-pause endings.
  text = text.replace(/([^.!?:;\s])(\n+)/g, "$1.$2");

  // 16. Clean up pipe-substitution artifacts: collapse runs of ", ..." and
  //     strip leading/trailing commas, plus collapse any whitespace.
  text = text.replace(/(?:,\s*){2,}/g, ", ");
  text = text.replace(/^[\s,]+|[\s,]+$/g, "");
  text = text.replace(/\s+/g, " ");

  return text.trim();
}

// Matches a base pictographic glyph plus any trailing variation selectors
// (U+FE0F) and ZWJ-joined pictographs, so combined emoji like 👨‍👩‍👧 and ❤️
// disappear cleanly instead of leaving dangling joiners or selectors behind.
const EMOJI_PATTERN = /\p{Extended_Pictographic}(?:\u{FE0F}|\u{200D}\p{Extended_Pictographic})*/gu;

/**
 * Strip emoji glyphs from text destined for TTS so the voice model doesn't
 * try to pronounce them. Safe to run on partial, mid-stream text.
 */
export function stripEmojisForTTS(input: string): string {
  return input.replace(EMOJI_PATTERN, "").replace(/\s+/g, " ").trim();
}
