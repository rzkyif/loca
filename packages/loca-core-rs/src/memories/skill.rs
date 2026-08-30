//! `SKILL.md`: an optional frontmatter block followed by the markdown body.
//!
//! The grammar is deliberately narrow and is not YAML: only `description` and
//! `suggested-tools` (inline `[a, b]` or a `- item` block list) are read;
//! every other key is tolerated and ignored so a community `SKILL.md` drops in
//! unchanged. The client's memory editor runs the same grammar.

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

/// Split a `SKILL.md` into its parts. No frontmatter means the whole text is
/// `body`.
pub fn parse_skill(content: &str) -> SkillParts {
    let Some((frontmatter, rest)) = split_frontmatter(content) else {
        return SkillParts {
            description: String::new(),
            suggested_tools: Vec::new(),
            body: content.to_string(),
        };
    };
    let (description, suggested_tools) = parse_frontmatter(frontmatter);
    // Drop the single blank line conventionally separating block and body.
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

/// `^---\r?\n([\s\S]*?)\r?\n---\r?\n?` as a scan: the block and what follows.
///
/// Lazy (ends at the first `---` after a newline) and the trailing `\r?\n?` is
/// optional, so `----` and `--- trailing` close the block too, leaving the
/// extra characters at the head of the body.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let after_open = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    // The closer needs a preceding newline, so `---\n---` is not a match.
    let nl = after_open.find("\n---")?;
    // A `\r` right before the newline belongs to the closer.
    let block_end = if after_open[..nl].ends_with('\r') {
        nl - 1
    } else {
        nl
    };
    let mut rest = &after_open[nl + 4..];
    rest = rest.strip_prefix('\r').unwrap_or(rest);
    rest = rest.strip_prefix('\n').unwrap_or(rest);
    Some((&after_open[..block_end], rest))
}

/// A repeated `description` wins on its last occurrence; `suggested-tools`
/// appends.
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
                    // Only one trailing `]` is dropped.
                    let inner = inline.strip_suffix(']').unwrap_or(inline);
                    for part in inner.split(',') {
                        let tool = unquote(part.trim());
                        if !tool.is_empty() {
                            suggested_tools.push(tool);
                        }
                    }
                } else {
                    // Block list: blank lines are skipped, anything else ends it.
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

/// `^([A-Za-z][\w-]*):\s*(.*)$` with ASCII `\w`; key lowercased, value trimmed.
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

/// `^\s*-\s*(.+)$`: a list item's text.
fn list_item(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('-')?;
    let text = rest.trim_start();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Strip at most one leading and one trailing quote, then trim.
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
