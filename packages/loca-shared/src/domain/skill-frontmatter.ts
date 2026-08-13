// Parse + serialize a skill's SKILL.md. A skill file is an optional frontmatter
// block (description + suggested-tools) followed by the markdown instruction
// body. The memory editor decomposes a skill into these three parts and
// recomposes the file on save; the core re-derives the summary source
// (description) and the suggested_tools list from the same frontmatter, so both
// sides must agree on the format. This module is that single source.
//
// The frontmatter grammar is intentionally narrow: only `description` (a scalar)
// and `suggested-tools` (an inline `[a, b]` array or a `- item` block list) are
// read. Any other key (notably Anthropic Agent Skills' `name`) is tolerated and
// ignored, so a community SKILL.md drops in unchanged.

const FRONTMATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;

export interface SkillParts {
  /** Frontmatter `description`, or "" when absent. Seeds relevance + listing. */
  description: string;
  /** Frontmatter `suggested-tools`, advisory tool names. */
  suggestedTools: string[];
  /** The markdown body after the frontmatter (and its trailing blank line). */
  body: string;
}

/** Split a SKILL.md into its description, suggested tools, and body. Files with
 *  no frontmatter return the whole text as `body`. */
export function parseSkill(content: string): SkillParts {
  const m = content.match(FRONTMATTER_RE);
  if (!m) return { description: "", suggestedTools: [], body: content };
  const { description, suggestedTools } = parseFrontmatter(m[1]);
  // Drop the frontmatter block, then the single blank line that conventionally
  // separates it from the body (the regex already consumed the closing `---`
  // and one newline, so the body still leads with that blank line's newline).
  const body = content.slice(m[0].length).replace(/^\r?\n/, "");
  return { description, suggestedTools, body };
}

/** Recompose a SKILL.md from its parts. Emits a frontmatter block only when
 *  there is a description or at least one tool; otherwise the body stands
 *  alone. The output round-trips through `parseSkill`. */
export function serializeSkill(parts: SkillParts): string {
  const description = parts.description.trim();
  const tools = parts.suggestedTools.map((t) => t.trim()).filter(Boolean);
  if (!description && tools.length === 0) return parts.body;
  const lines = ["---"];
  // Always double-quote the description: the parser strips at most one leading
  // and one trailing quote, so wrapping round-trips for any single-line value,
  // including ones that contain colons or their own quotes.
  if (description) lines.push(`description: "${description}"`);
  if (tools.length > 0) {
    lines.push("suggested-tools:");
    for (const t of tools) lines.push(`  - ${t}`);
  }
  lines.push("---");
  return `${lines.join("\n")}\n\n${parts.body}`;
}

// --- internals --------------------------------------------------------------

function parseFrontmatter(text: string): { description: string; suggestedTools: string[] } {
  const lines = text.split(/\r?\n/);
  let description = "";
  const suggestedTools: string[] = [];
  for (let i = 0; i < lines.length; i++) {
    const kv = lines[i].match(/^([A-Za-z][\w-]*):\s*(.*)$/);
    if (!kv) continue;
    const key = kv[1].toLowerCase();
    const value = kv[2].trim();
    if (key === "description") {
      description = unquote(value);
    } else if (key === "suggested-tools" || key === "suggestedtools") {
      if (value.startsWith("[")) {
        // Inline array: [a, "b", c]
        for (const part of value.replace(/^\[|\]$/g, "").split(",")) {
          const t = unquote(part.trim());
          if (t) suggestedTools.push(t);
        }
      } else {
        // Block list: subsequent `- item` lines. Blank lines are skipped; the
        // list ends at the next `key:` line or any other non-list content.
        for (let j = i + 1; j < lines.length; j++) {
          if (lines[j].trim() === "") continue;
          const item = lines[j].match(/^\s*-\s*(.+)$/);
          if (!item) break;
          const t = unquote(item[1].trim());
          if (t) suggestedTools.push(t);
        }
      }
    }
  }
  return { description, suggestedTools };
}

function unquote(s: string): string {
  return s.replace(/^["']|["']$/g, "").trim();
}
