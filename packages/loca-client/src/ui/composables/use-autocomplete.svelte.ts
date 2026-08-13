/**
 * Drives the `@trigger` autocomplete dropdown for the message input: tracks the
 * open/selection state, detects the token being typed before the caret, anchors
 * the popup under the caret, and handles the arrow/enter/tab/escape key nav.
 *
 * Following the convention of use-blink (the consuming component owns the
 * `$effect`/lifecycle), this class holds only the dropdown mechanics. The
 * option list itself stays a `$derived.by` in the component, since it reads two
 * stores (snippets + memories); the component passes that list into the key
 * handler and the clamp effect. DOM refs (the textarea and the sizing mirror
 * span) are supplied once via `bind`, because caret measurement needs them.
 */

import { TRIGGER_BEFORE_CARET } from "$lib/snippets/snippets";

// One dropdown serves snippets (expanded client-side at send) and `@` memories
// (token stays in the message; the core injects the content at generation
// time). The typed symbol (`#`/`@`/`/`) selects which list is shown. On a
// trigger collision the snippet wins.
export type AutocompleteOption = {
  id: string;
  name: string;
  trigger: string;
  source: "snippet" | "memory" | "mcp_prompt" | "resource";
};

// Collects every `#`/`@`/`/` trigger token already present in `source`,
// excluding the one spanning [excludeStart, excludeEnd) (the token currently
// being typed, so it doesn't filter itself out of the suggestions). A bare name
// is any non-whitespace run (so a memory like `@ext/skills/foo` is one token),
// minus a trailing sentence-punctuation mark; the quoted `@"name with spaces"`
// form is only needed to span whitespace. Used to hide already-applied entries.
const TRIGGER_EXISTING = /(^|[^\w@#/])([#@/](?:"[^"]+"|[^\s"]*[^\s".,:;!?)\]}']))/g;

export function collectExistingTriggers(
  source: string,
  excludeStart: number,
  excludeEnd: number,
): Set<string> {
  const found = new Set<string>();
  TRIGGER_EXISTING.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = TRIGGER_EXISTING.exec(source)) !== null) {
    const tokenStart = match.index + match[1].length;
    const tokenEnd = tokenStart + match[2].length;
    if (tokenEnd <= excludeStart || tokenStart >= excludeEnd) {
      found.add(match[2].toLowerCase());
    }
  }
  return found;
}

export class Autocomplete {
  open = $state(false);
  prefix = $state("");
  index = $state(0);
  anchor = $state<{ top: number; left: number }>({ top: 0, left: 0 });
  triggerStart = $state(-1);
  triggerEnd = $state(-1);
  imeComposing = $state(false);

  private getTextarea: () => HTMLTextAreaElement | undefined = () => undefined;
  private getMirror: () => HTMLSpanElement | undefined = () => undefined;

  /** Supply the live DOM refs caret measurement reads. */
  bind(
    getTextarea: () => HTMLTextAreaElement | undefined,
    getMirror: () => HTMLSpanElement | undefined,
  ): void {
    this.getTextarea = getTextarea;
    this.getMirror = getMirror;
  }

  // Anchors the dropdown under the caret by measuring a Range on the sibling
  // sizing span. It shares the textarea's grid cell and wraps identically, so
  // we can read the caret position directly without cloning styles into a
  // hidden mirror.
  private measureCaretAt(index: number): { top: number; left: number } {
    const mirror = this.getMirror();
    const ta = this.getTextarea();
    if (!mirror || !ta) return { top: 0, left: 0 };
    const textNode = mirror.firstChild;
    if (!textNode) return { top: 0, left: 0 };
    const range = document.createRange();
    const safeIndex = Math.min(index, textNode.textContent?.length ?? 0);
    range.setStart(textNode, safeIndex);
    range.setEnd(textNode, safeIndex);
    const rect = range.getBoundingClientRect();
    const cs = window.getComputedStyle(ta);
    const lineHeight = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.2;
    return { top: rect.top + lineHeight + 4, left: rect.left };
  }

  /** Recompute open state / prefix / anchor from the current input + caret. */
  updateFromInput(text: string): void {
    const ta = this.getTextarea();
    if (this.imeComposing || !ta) {
      this.open = false;
      return;
    }
    const caret = ta.selectionStart ?? text.length;
    const before = text.slice(0, caret);
    const match = before.match(TRIGGER_BEFORE_CARET);
    if (!match) {
      this.open = false;
      return;
    }
    const token = match[1];
    // Only reset the selected index when the filter prefix actually changed
    // (or we're freshly opening). Otherwise arrow-key presses, which fire
    // onkeyup -> here, would bounce the highlight back to the first option.
    const prefixChanged = !this.open || token !== this.prefix;
    this.prefix = token;
    this.triggerStart = caret - token.length;
    this.triggerEnd = caret;
    if (prefixChanged) this.index = 0;
    this.anchor = this.measureCaretAt(this.triggerStart);
    this.open = true;
  }

  /**
   * Replace the in-progress token with `trigger` (plus a trailing space) and
   * close the dropdown. Returns the new text and the caret offset to restore;
   * the caller owns the textarea ref, so it assigns the text and refocuses.
   */
  applyTrigger(text: string, trigger: string): { text: string; caret: number } | null {
    const start = this.triggerStart;
    const end = this.triggerEnd;
    this.open = false;
    if (start < 0 || end < 0) return null;
    const before = text.slice(0, start);
    const after = text.slice(end);
    const replacement = `${trigger} `;
    return {
      text: `${before}${replacement}${after}`,
      caret: start + replacement.length,
    };
  }

  onCompositionStart(): void {
    this.imeComposing = true;
    this.open = false;
  }

  onCompositionEnd(text: string): void {
    this.imeComposing = false;
    this.updateFromInput(text);
  }

  /**
   * Handle a keydown while the dropdown may be open. Returns true if the key
   * was consumed (so the caller skips its own Enter-to-send handling).
   * Enter/Tab commit the highlighted option through `onSelect`.
   */
  handleKey(
    e: KeyboardEvent,
    options: AutocompleteOption[],
    onSelect: (option: AutocompleteOption) => void,
  ): boolean {
    if (!this.open || options.length === 0) return false;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      this.index = (this.index + 1) % options.length;
      return true;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      this.index = (this.index - 1 + options.length) % options.length;
      return true;
    }
    if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      const selected = options[this.index];
      if (selected) onSelect(selected);
      return true;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      this.open = false;
      return true;
    }
    return false;
  }

  /** Keep the selection in range as the option list changes. Call from a
   *  `$effect` that reads the derived option count. */
  clampIndex(optionCount: number): void {
    if (!this.open) return;
    if (optionCount === 0) {
      this.open = false;
      return;
    }
    if (this.index >= optionCount) this.index = 0;
  }
}

export function useAutocomplete(): Autocomplete {
  return new Autocomplete();
}
