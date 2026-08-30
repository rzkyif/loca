// Guards the "every website rendition is in lockstep with the client at default
// settings" rule for the chat composer specifically.
//
// `UserInputView` OWNS the canonical default composition, so a bare
// `<UserInputView .../>` renders exactly what the client shows by default. The
// props below RE-DECIDE which controls appear and exist as the CLIENT's
// live-override seams; the website setting them is what makes its renditions
// drift. Props that only feed the always-present controls scripted STATE
// (`value`, `placeholder`, `voiceClass`, ...) are allowed.
//
// Wired into `deno task lint`. If a website demo genuinely needs a control the
// client hides at default settings, change the client default, never
// hand-configure the shell here.

import { walk } from "@std/fs/walk";
import { fromFileUrl } from "@std/path";

// Native OS path (fromFileUrl); URL .pathname breaks walk()/readdir on Windows.
const ROOT = fromFileUrl(new URL("../../packages/loca-website/src/", import.meta.url));
const REL = "packages/loca-website/src/";

// Composition-deciding props: the client's live-override seams.
const FORBIDDEN = ["belowContent", "showVoice", "showLeftGroup", "contentOverride", "rightSlot"];

interface Violation {
  file: string;
  prop: string;
}

// Return the opening-tag text (`<UserInputView ... >`) starting at `start`,
// tracking brace/quote nesting so a `>` inside an expression (e.g. an arrow
// `() => x`) or a string isn't mistaken for the tag's end.
function openTag(text: string, start: number): string {
  let depth = 0;
  let quote = "";
  let out = "";
  for (let i = start; i < text.length; i++) {
    const c = text[i];
    out += c;
    if (quote) {
      if (c === quote) quote = "";
    } else if (c === '"' || c === "'" || c === "`") {
      quote = c;
    } else if (c === "{") depth++;
    else if (c === "}") depth--;
    else if (c === ">" && depth === 0) break;
  }
  return out;
}

async function scan(): Promise<Violation[]> {
  const violations: Violation[] = [];
  for await (const entry of walk(ROOT, { exts: [".svelte"], includeDirs: false })) {
    const text = await Deno.readTextFile(entry.path);
    // walk yields native backslash paths on Windows; normalize so indexOf(REL)
    // (a forward-slash needle) finds the repo-relative tail on every OS.
    const p = entry.path.replaceAll("\\", "/");
    const rel = p.slice(p.indexOf(REL));
    let from = 0;
    for (;;) {
      const idx = text.indexOf("<UserInputView", from);
      if (idx < 0) break;
      from = idx + 1;
      // Only a real element (next char is whitespace or the self-close), not a
      // substring like `<UserInputViewFoo`.
      if (!/[\s/>]/.test(text[idx + "<UserInputView".length] ?? "")) continue;
      const tag = openTag(text, idx);
      for (const prop of FORBIDDEN) {
        // The prop as an attribute name: a boundary before it, then `=`, `/`,
        // `>`, or whitespace after (covers `prop={...}` and boolean `prop`).
        if (new RegExp(`(?:^|[\\s])${prop}(?=[\\s=/>])`).test(tag)) {
          violations.push({ file: rel, prop });
        }
      }
    }
  }
  return violations;
}

const violations = await scan();
if (violations.length > 0) {
  console.error(
    "Website UserInputView renditions may not set composition props " +
      "(they decide which controls appear and would drift from the client):",
  );
  for (const v of violations) console.error(`  ${v.file}  sets \`${v.prop}\``);
  console.error(
    "\nUserInputView already renders the canonical default composition (quick-" +
      "model bar + Voice Input per stt.enabled). Drop the prop and render it bare; " +
      "feed only scripted STATE (value, voiceClass, vadListening, ...). To change " +
      "what appears by default, change the client default + the View, not this call.",
  );
  Deno.exit(1);
}
