//! `SKILL.md`: an optional frontmatter block followed by the markdown
//! instruction body.
//!
//! Ported from `@loca/shared/src/domain/skill-frontmatter.ts`, which is the
//! single source both sides agree on: the client's memory editor decomposes a
//! skill into these parts and recomposes the file on save, and the core
//! re-derives the summary source (`description`) and the advisory
//! `suggested-tools` list from the same block.
//!
//! Only the parse direction is ported. `serializeSkill` has no core-side caller:
//! the core never writes a `SKILL.md` it composed itself, it writes whatever the
//! client or the user gave it.
//!
//! The grammar is deliberately narrow and is **not** YAML. Only `description`
//! (a scalar) and `suggested-tools` (an inline `[a, b]` array or a `- item`
//! block list) are read; every other key, notably Anthropic Agent Skills'
//! `name`, is tolerated and ignored so a community `SKILL.md` drops in
//! unchanged. Reaching for a YAML crate here would parse strictly *more* than
//! this does and would start rejecting files the TypeScript accepts.

/// The three parts of a `SKILL.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillParts {
    /// Frontmatter `description`, or empty when absent. Seeds the summary.
    pub description: String,
    /// Frontmatter `suggested-tools`, advisory tool names.
    pub suggested_tools: Vec<String>,
    /// The markdown body after the frontmatter and its trailing blank line.
    pub body: String,
}

/// Split a `SKILL.md` into its description, suggested tools, and body. A file
/// with no frontmatter returns the whole text as `body`.
pub fn parse_skill(content: &str) -> SkillParts {
    let Some((frontmatter, rest)) = split_frontmatter(content) else {
        return SkillParts {
            description: String::new(),
            suggested_tools: Vec::new(),
            body: content.to_string(),
        };
    };
    let (description, suggested_tools) = parse_frontmatter(frontmatter);
    // Drop the single blank line that conventionally separates the block from
    // the body. The closing delimiter's own newline is already consumed.
    let body = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    SkillParts {
        description,
        suggested_tools,
        body: body.to_string(),
    }
}

/// `^---\r?\n([\s\S]*?)\r?\n---\r?\n?` as a scan, returning the captured block
/// and everything after the match.
///
/// Two properties of that pattern decide what closes a block, and both are easy
/// to tighten by accident:
///
/// - The laziness: the block ends at the **first** `---` after a newline, not
///   the last.
/// - The trailing `\r?\n?` is **optional**, so `---` does not have to end its
///   line. `----` and `--- trailing` close the block too, leaving the extra
///   characters at the head of the body. Requiring a line end here would reject
///   files the shared `parseSkill` accepts, and since the client's memory editor
///   runs that same parser, the two would disagree about one `SKILL.md`: no
///   seeded summary and no `suggestedTools` on the core side only.
///
/// Written as a scan rather than a regex so the `\r` handling is visible.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let after_open = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    // The closer is preceded by a newline, so an opening `---` immediately
    // followed by `---` on the next line is not a match: the lazy group needs at
    // least that newline.
    let nl = after_open.find("\n---")?;
    // `\r?\n---`: a `\r` immediately before the newline belongs to the closer,
    // not to the captured block.
    let block_end = if after_open[..nl].ends_with('\r') {
        nl - 1
    } else {
        nl
    };
    // `\r?\n?` after the delimiter, each taken only if present.
    let mut rest = &after_open[nl + 4..];
    rest = rest.strip_prefix('\r').unwrap_or(rest);
    rest = rest.strip_prefix('\n').unwrap_or(rest);
    Some((&after_open[..block_end], rest))
}

/// The two keys that are read, from a block whose other keys are ignored.
///
/// A repeated key wins on its last occurrence for `description`, and appends for
/// `suggested-tools`, which is what a linear scan over the lines gives and what
/// the TypeScript does.
fn parse_frontmatter(text: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut description = String::new();
    let mut suggested_tools = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((key, value)) = split_key_value(line) else {
            continue;
        };
        match key.as_str() {
            "description" => description = unquote(value),
            "suggested-tools" | "suggestedtools" => {
                if let Some(inline) = value.strip_prefix('[') {
                    // `[a, "b", c]`. Only one trailing `]` is dropped, matching
                    // a `replace(/^\[|\]$/g, "")`.
                    let inner = inline.strip_suffix(']').unwrap_or(inline);
                    for part in inner.split(',') {
                        let tool = unquote(part.trim());
                        if !tool.is_empty() {
                            suggested_tools.push(tool);
                        }
                    }
                } else {
                    // A block list: the following `- item` lines. Blank lines are
                    // skipped; anything else ends the list.
                    for next in &lines[i + 1..] {
                        if next.trim().is_empty() {
                            continue;
                        }
                        let Some(item) = list_item(next) else { break };
                        let tool = unquote(item.trim());
                        if !tool.is_empty() {
                            suggested_tools.push(tool);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (description, suggested_tools)
}

/// `^([A-Za-z][\w-]*):\s*(.*)$`, with the JavaScript meaning of `\w`
/// (`[A-Za-z0-9_]`, ASCII). The key is lowercased and the value trimmed.
///
/// Rust's `\w` is Unicode, so writing this as a regex would silently accept keys
/// the TypeScript rejects. Scanning the bytes keeps the two identical and needs
/// no escape.
fn split_key_value(line: &str) -> Option<(String, &str)> {
    let mut chars = line.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut colon = None;
    for (idx, c) in chars {
        if c == ':' {
            colon = Some(idx);
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return None;
        }
    }
    let colon = colon?;
    let key = line[..colon].to_lowercase();
    Some((key, line[colon + 1..].trim()))
}

/// `^\s*-\s*(.+)$`: a list item's text, or `None` when the line is not one.
fn list_item(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('-')?;
    let text = rest.trim_start();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Strip at most one leading and one trailing quote, then trim. Wrapping a
/// value in double quotes therefore round-trips for any single-line value,
/// including one containing colons or its own quotes.
fn unquote(s: &str) -> String {
    let mut out = s;
    if let Some(rest) = out.strip_prefix(['"', '\'']) {
        out = rest;
    }
    if let Some(rest) = out.strip_suffix(['"', '\'']) {
        out = rest;
    }
    out.trim().to_string()
}
