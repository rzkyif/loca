//! Stream parser for Deno's interactive permission prompt as it appears on a
//! tool worker's PTY. Pure state machine over text chunks; no I/O, so it is
//! unit-testable against recorded transcripts.
//!
//! Ports `packages/loca-core/src/extensions/prompt-parser.ts`.
//!
//! A prompt block looks like this, with the ANSI styling stripped:
//!
//! ```text
//! ┏ ⚠️  Deno requests read access to "/etc/hosts".
//! ┠─ Requested by `Deno.readFileSync()` API.
//! ┠─ To see a stack trace for this prompt, set the DENO_TRACE_PERMISSIONS ...
//! ┠─ Learn more at: https://docs.deno.com/go/--allow-read
//! ┠─ Run again with --allow-read to bypass this prompt.
//! ┗ Allow? [y/n/A] (y = yes, allow; ...) >
//! ```
//!
//! The final line has NO trailing newline, because Deno is blocked reading the
//! answer, so prompt completion is detected on the PARTIAL line buffer. A port
//! that only inspects completed lines deadlocks on every prompt, and the symptom
//! is a ten-second hang per tool call. After the answer is accepted Deno erases
//! the block with cursor moves and prints one confirmation line, `✅ Granted ...`
//! or `❌ Denied ...`.
//!
//! Anything that is not prompt grammar is forwarded as a stderr line, exactly
//! like the piped-stderr path: the same PTY carries the tool's own stderr.
//!
//! The wording is not a stable Deno API. The patterns anchor on the substrings
//! that have survived every Deno 2.x release ("Deno requests", "Allow?",
//! "Granted"/"Denied"), the deno version is pinned on every channel, and the
//! live probe re-validates the format against the real binary.
//!
//! # Difference from the TypeScript, and why
//!
//! `feed` RETURNS its events rather than invoking a callback. The callback
//! exists in TypeScript because a closure is the only seam available there; a
//! returned vector is easier to test and removes the re-entrancy question
//! entirely.

use std::sync::OnceLock;

use regex::Regex;

/// Deno caps prompt content at 10 KB, so anything larger is not a real prompt.
/// Hitting this bails the block back to idle, which is what stops ordinary
/// output being swallowed as prompt chrome forever.
const MAX_BLOCK_BYTES: usize = 16_384;

/// Bound on the partial (pre-newline) line buffer. A tool sharing the PTY can
/// write an endless stream with no newline; without this cap the buffer grows
/// without limit and exhausts Core's memory. A different failure from
/// [`MAX_BLOCK_BYTES`] with a different fix, so the two are pinned separately.
///
/// The TypeScript counts UTF-16 code units here and this counts bytes. The
/// prompt text carries multi-byte box-drawing characters and emoji, so the two
/// differ; the cap is a memory bound rather than a wire value, and a test
/// pinning the exact boundary would be pinning a JavaScript encoding detail.
const MAX_PARTIAL_BYTES: usize = 1_000_000;

const TRUNCATION_MARKER: &str = " …[truncated]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptParserEvent {
    StderrLine {
        line: String,
    },
    /// `permission` is the RAW kind string from the prompt. It is usually one of
    /// the seven `loca.json` kinds, but Deno also prompts for `import`; the
    /// matcher is what maps it. `resource` is empty for a blanket request with
    /// no quoted resource.
    Prompt {
        permission: String,
        resource: String,
        api_name: Option<String>,
    },
    Settled {
        granted: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    InPrompt,
    Awaiting,
}

pub struct PromptParser {
    buf: String,
    state: State,
    permission: String,
    resource: String,
    api_name: Option<String>,
    block_bytes: usize,
}

impl Default for PromptParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptParser {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            state: State::Idle,
            permission: String::new(),
            resource: String::new(),
            api_name: None,
            block_bytes: 0,
        }
    }

    pub fn feed(&mut self, text: &str) -> Vec<PromptParserEvent> {
        let mut out = Vec::new();
        self.buf.push_str(text);
        // Detach the buffer so the completed lines can be handled while the
        // remainder is written back. Everything before the last `\n` is a
        // complete line; what follows it is the new partial.
        let buf = std::mem::take(&mut self.buf);
        let mut parts: Vec<&str> = buf.split('\n').collect();
        let rest = parts.pop().unwrap_or_default();
        for line in parts {
            self.handle_line(&strip_ansi(line), &mut out);
        }
        rest.clone_into(&mut self.buf);

        // The Allow? line never gets a newline while Deno waits for the answer,
        // so the partial buffer is where completion is detected. An ANSI
        // sequence split across chunks strips incompletely at worst, and the
        // next chunk completes it.
        if self.state == State::InPrompt {
            self.block_bytes += text.len();
            if self.block_bytes > MAX_BLOCK_BYTES {
                // Not a real prompt. Bail so ordinary output stops being
                // swallowed as block chrome.
                self.state = State::Idle;
                return out;
            }
            let partial = strip_ansi(&self.buf);
            if allow_tail_re().is_match(&partial) {
                self.state = State::Awaiting;
                out.push(self.prompt_event());
            }
        }

        // Runaway no-newline output, in the idle or awaiting state: flush one
        // truncated stderr line and drop the buffer so memory stays bounded. The
        // in_prompt state cannot reach here with a large buffer, because
        // MAX_BLOCK_BYTES bails it to idle first.
        if self.buf.len() > MAX_PARTIAL_BYTES {
            let flushed = strip_ansi(&self.buf);
            self.buf.clear();
            if !flushed.trim().is_empty() {
                out.push(PromptParserEvent::StderrLine {
                    line: format!(
                        "{}{TRUNCATION_MARKER}",
                        truncate(&flushed, MAX_PARTIAL_BYTES)
                    ),
                });
            }
        }
        out
    }

    fn prompt_event(&self) -> PromptParserEvent {
        PromptParserEvent::Prompt {
            permission: self.permission.clone(),
            resource: self.resource.clone(),
            api_name: self.api_name.clone(),
        }
    }

    /// Settle on a Granted/Denied confirmation; true when it settled.
    fn check_settle(&mut self, line: &str, out: &mut Vec<PromptParserEvent>) -> bool {
        if line.contains('✅') && line.contains("Granted") {
            self.state = State::Idle;
            out.push(PromptParserEvent::Settled { granted: true });
            return true;
        }
        if line.contains('❌') && line.contains("Denied") {
            self.state = State::Idle;
            out.push(PromptParserEvent::Settled { granted: false });
            return true;
        }
        false
    }

    fn handle_line(&mut self, line: &str, out: &mut Vec<PromptParserEvent>) {
        if line.trim().is_empty() {
            return;
        }
        match self.state {
            State::Idle => {
                if let Some(start) = prompt_start_re().captures(line) {
                    self.state = State::InPrompt;
                    self.permission = start[1].to_string();
                    self.resource = start
                        .get(2)
                        .map_or(String::new(), |m| m.as_str().to_string());
                    self.api_name = None;
                    self.block_bytes = line.len();
                    return;
                }
                out.push(PromptParserEvent::StderrLine {
                    line: line.to_string(),
                });
            }
            State::InPrompt => {
                if let Some(api) = api_line_re().captures(line) {
                    self.api_name = Some(api[1].to_string());
                    return;
                }
                // The Allow? line normally surfaces through the partial-buffer
                // check in `feed` (it carries no newline while Deno waits), but
                // buffering can also deliver it as a completed line, possibly
                // with the confirmation glued on after the "> ". Emit the prompt
                // here too, then let the same line settle if it already carries
                // the verdict.
                if line.contains("Allow?") {
                    self.state = State::Awaiting;
                    out.push(self.prompt_event());
                    self.check_settle(line, out);
                    return;
                }
                // Box-drawing lines are prompt chrome (stack frames, doc links).
                // The tool's JS thread is blocked during a prompt, so genuine
                // tool stderr cannot interleave here; anything unprefixed is
                // Deno's own (a clear_stdin error, say) and still surfaces.
                if is_block_chrome(line) {
                    return;
                }
                out.push(PromptParserEvent::StderrLine {
                    line: line.to_string(),
                });
            }
            State::Awaiting => {
                if self.check_settle(line, out) {
                    return;
                }
                // The Allow? line completes (gains its newline) once the answer
                // is read, and may be reprinted after a flushed or garbled
                // answer. Swallow it and the rest of the block chrome.
                if line.contains("Allow?") || is_block_chrome(line) {
                    return;
                }
                // A console (Windows ConPTY) echoes the typed answer back while
                // Deno reads it. The tool's thread is still blocked at this
                // point, so a bare y/n here can only be our own answer bouncing
                // back, never tool output. That argument is the whole reason
                // this is not an injection hole, so it stays written down.
                let trimmed = line.trim();
                if trimmed.len() == 1 && trimmed.starts_with(['y', 'Y', 'n', 'N']) {
                    return;
                }
                out.push(PromptParserEvent::StderrLine {
                    line: line.to_string(),
                });
            }
        }
    }
}

/// Cut at a character boundary at or below `max`, so a multi-byte character is
/// never split. The TypeScript slices UTF-16 code units, where the same problem
/// cannot arise.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn strip_ansi(text: &str) -> String {
    ansi_re().replace_all(text, "").replace('\r', "")
}

fn is_block_chrome(line: &str) -> bool {
    line.starts_with(['┏', '┠', '┃', '┗', '├', '└'])
}

macro_rules! lazy_re {
    ($name:ident, $pattern:expr) => {
        // Literal patterns, so a compile failure here is a build-time bug rather
        // than anything a running core can hit.
        #[allow(clippy::unwrap_used)]
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).unwrap())
        }
    };
}

// CSI and OSC sequences. `\r` is dropped separately, in `strip_ansi`.
lazy_re!(
    ansi_re,
    r"\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))"
);
lazy_re!(
    prompt_start_re,
    r#"Deno requests (\w+) access(?: to "([^"]*)")?"#
);
lazy_re!(api_line_re, r"Requested by `(.+)` API");
lazy_re!(allow_tail_re, r"Allow\?.*>\s*$");

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const B: &str = "\x1b[1m"; // bold
    const R: &str = "\x1b[0m"; // reset

    /// Faithful to the real block recorded from deno 2.8.2 on a PTY: styled
    /// segments, box-drawing prefixes, and the final Allow? line WITHOUT a
    /// trailing newline.
    fn prompt_block(kind: &str, resource: &str, api: Option<&str>) -> String {
        let mut s =
            format!("┏ ⚠️  {B}Deno requests {R}{B}{kind} access to \"{resource}\"{R}{B}.{R}\n");
        if let Some(api) = api {
            s += &format!("┠─ Requested by `{B}{api}{R}` API.\n");
        }
        s += "┠─ To see a stack trace for this prompt, set the DENO_TRACE_PERMISSIONS environmental variable.\n";
        s += &format!(
            "┠─ {R}\x1b[3mLearn more at: {R}\x1b[4m\x1b[36mhttps://docs.deno.com/go/--allow-{kind}{R}{R}\n"
        );
        s += &format!("┠─ {R}\x1b[3mRun again with --allow-{kind} to bypass this prompt.{R}\n");
        s += &format!(
            "┗ {B}Allow?{R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all {kind} permissions) > "
        );
        s
    }

    fn granted_line(kind: &str, resource: &str) -> String {
        // Deno erases the block with cursor-up + erase-line sequences first.
        format!("\x1b[1A\x1b[2K\x1b[1A\x1b[2K✅ {B}Granted {kind} access to \"{resource}\".{R}\n")
    }

    fn denied_line(kind: &str, resource: &str) -> String {
        format!("\x1b[1A\x1b[2K❌ {B}Denied {kind} access to \"{resource}\".{R}\n")
    }

    fn prompt(permission: &str, resource: &str, api: Option<&str>) -> PromptParserEvent {
        PromptParserEvent::Prompt {
            permission: permission.to_string(),
            resource: resource.to_string(),
            api_name: api.map(str::to_string),
        }
    }

    fn stderr(line: &str) -> PromptParserEvent {
        PromptParserEvent::StderrLine {
            line: line.to_string(),
        }
    }

    #[test]
    fn a_prompt_completes_on_the_partial_line_and_then_settles() {
        // The Allow? line carries no newline while Deno waits for the answer.
        // A parser that only inspects completed lines never emits this event,
        // and every tool call hangs for the full give-up budget.
        let mut p = PromptParser::new();
        let events = p.feed(&prompt_block(
            "read",
            "/etc/hosts",
            Some("Deno.readFileSync()"),
        ));
        assert_eq!(
            events,
            vec![prompt("read", "/etc/hosts", Some("Deno.readFileSync()"))]
        );
        assert_eq!(
            p.feed(&granted_line("read", "/etc/hosts")),
            vec![PromptParserEvent::Settled { granted: true }]
        );
    }

    #[test]
    fn a_denied_confirmation_settles_false() {
        let mut p = PromptParser::new();
        assert_eq!(
            p.feed(&prompt_block("write", "/tmp/x.txt", None)),
            vec![prompt("write", "/tmp/x.txt", None)]
        );
        assert_eq!(
            p.feed(&denied_line("write", "/tmp/x.txt")),
            vec![PromptParserEvent::Settled { granted: false }]
        );
    }

    #[test]
    fn a_blanket_prompt_without_a_quoted_resource_still_prompts() {
        // Deno.env.toObject(), for instance: no `to "..."` segment at all.
        let mut p = PromptParser::new();
        let events = p.feed(&format!(
            "┏ ⚠️  {B}Deno requests env access{R}{B}.{R}\n┗ {B}Allow?{R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all env permissions) > "
        ));
        assert_eq!(events, vec![prompt("env", "", None)]);
    }

    #[test]
    fn stderr_lines_pass_through_around_prompts() {
        let mut p = PromptParser::new();
        assert_eq!(p.feed("tool log line A\n"), vec![stderr("tool log line A")]);
        assert_eq!(
            p.feed(&prompt_block("net", "example.com:443", Some("fetch()"))),
            vec![prompt("net", "example.com:443", Some("fetch()"))]
        );
        assert_eq!(
            p.feed(&granted_line("net", "example.com:443")),
            vec![PromptParserEvent::Settled { granted: true }]
        );
        assert_eq!(p.feed("tool log line B\n"), vec![stderr("tool log line B")]);
    }

    #[test]
    fn sequential_prompts_in_one_stream_stay_separate() {
        let mut p = PromptParser::new();
        let mut events = Vec::new();
        events.extend(p.feed(&prompt_block("read", "/a", None)));
        events.extend(p.feed(&granted_line("read", "/a")));
        events.extend(p.feed(&prompt_block("read", "/b", None)));
        events.extend(p.feed(&denied_line("read", "/b")));
        assert_eq!(
            events,
            vec![
                prompt("read", "/a", None),
                PromptParserEvent::Settled { granted: true },
                prompt("read", "/b", None),
                PromptParserEvent::Settled { granted: false },
            ]
        );
    }

    #[test]
    fn a_reprinted_allow_line_after_a_garbled_answer_is_swallowed() {
        let mut p = PromptParser::new();
        let mut events = p.feed(&prompt_block("run", "ls", None));
        // Unrecognized option: Deno erases and reprints the Allow line, so the
        // previous one now completes with a newline.
        events.extend(p.feed(&format!(
            "\n┗ {B}Unrecognized option. Allow?{R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all run permissions) > "
        )));
        events.extend(p.feed(&granted_line("run", "ls")));
        assert_eq!(
            events,
            vec![
                prompt("run", "ls", None),
                PromptParserEvent::Settled { granted: true },
            ]
        );
    }

    /// A pseudoconsole (Windows ConPTY) reflects a screen buffer: it wraps
    /// output in cursor-hide/show, per-line absolute cursor moves plus
    /// erase-line, and CRLF endings. Synthetic, because real ConPTY output can
    /// only be captured on Windows, and there is no Windows CI; it locks in that
    /// the VT chrome ConPTY adds is inert to the parser.
    fn conpty_wrap(block: &str) -> String {
        const HIDE: &str = "\x1b[?25l";
        const SHOW: &str = "\x1b[?25h";
        let lines: Vec<&str> = block.split('\n').collect();
        let has_tail = lines.last().is_some_and(|l| !l.is_empty());
        let mut out = HIDE.to_string();
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() && i == lines.len() - 1 {
                continue;
            }
            let last = i == lines.len() - 1 || (i == lines.len() - 2 && !has_tail);
            out += &format!("\x1b[{};1H\x1b[K{line}", i + 1);
            if !(last && has_tail) {
                out += "\r\n";
            }
        }
        out + SHOW
    }

    #[test]
    fn a_conpty_wrapped_prompt_parses_and_the_echoed_answer_is_swallowed() {
        let mut p = PromptParser::new();
        assert_eq!(
            p.feed(&conpty_wrap(&prompt_block(
                "net",
                "example.com:443",
                Some("fetch()")
            ))),
            vec![prompt("net", "example.com:443", Some("fetch()"))]
        );
        // The console echoes the typed answer while Deno reads it. Here it is
        // still glued to the unterminated Allow? line, so the Allow? rule is
        // what swallows it; the bare-line case is
        // `an_echoed_bare_answer_is_swallowed_rather_than_leaked`.
        assert_eq!(p.feed("y\r\n"), vec![]);
        assert_eq!(
            p.feed(&conpty_wrap(&granted_line("net", "example.com:443"))),
            vec![PromptParserEvent::Settled { granted: true }]
        );
    }

    #[test]
    fn an_echoed_bare_answer_is_swallowed_rather_than_leaked() {
        // Observed live on Windows: a bare "y"/"n" line of its own between the
        // prompt and the confirmation, because the console echoes what was
        // typed. It is our own answer bouncing back, never tool output (the
        // tool's JS thread is blocked for the whole prompt), so it must not
        // surface as a stderr line the user sees.
        //
        // The ConPTY test above cannot cover this: there the echo is glued to
        // the still-unterminated Allow? line, so the Allow? rule swallows it and
        // this rule is never reached.
        let mut p = PromptParser::new();
        p.feed(&prompt_block("net", "example.com:443", Some("fetch()")));
        // The newline completes the Allow? line, then the echo arrives alone.
        assert_eq!(p.feed("\r\n"), vec![]);
        for answer in ["y\r\n", "n\r\n", "Y\r\n", " n \r\n"] {
            assert_eq!(p.feed(answer), vec![], "the echo of {answer:?} leaked");
        }
        // Anything longer than a single character is real output and does
        // surface, which is what stops this rule swallowing a tool's log line.
        assert_eq!(p.feed("yes really\r\n"), vec![stderr("yes really")]);
    }

    #[test]
    fn chunking_does_not_change_the_events() {
        // Frame boundaries land mid-line and mid-escape-sequence in production,
        // which is exactly when an ANSI strip is incomplete on one pass.
        let transcript = format!(
            "boot noise\n{}{}mid noise\n{}{}tail noise\n",
            prompt_block("read", "/etc/hosts", Some("Deno.readFileSync()")),
            granted_line("read", "/etc/hosts"),
            prompt_block("net", "api.example.com:443", Some("fetch()")),
            denied_line("net", "api.example.com:443"),
        );
        let reference = PromptParser::new().feed(&transcript);
        assert_eq!(reference.len(), 7, "unexpected reference: {reference:?}");

        let bytes = transcript.as_bytes();
        // Deterministic rather than random: a size-varying sweep over every
        // chunk width covers the same boundaries and cannot pass one run and
        // fail the next.
        for width in 1..=37usize {
            let mut p = PromptParser::new();
            let mut events = Vec::new();
            let mut start = 0;
            while start < bytes.len() {
                let mut end = (start + width).min(bytes.len());
                while !transcript.is_char_boundary(end) {
                    end += 1;
                }
                events.extend(p.feed(&transcript[start..end]));
                start = end;
            }
            assert_eq!(events, reference, "chunk width {width} diverged");
        }
    }

    #[test]
    fn runaway_no_newline_output_is_truncated_rather_than_buffered_forever() {
        // A tool streams more than a megabyte with no newline. The partial
        // buffer must not grow without bound.
        let mut p = PromptParser::new();
        let mut events = Vec::new();
        for _ in 0..120 {
            events.extend(p.feed(&"x".repeat(10_000)));
        }
        let flushed = events
            .iter()
            .find(|e| matches!(e, PromptParserEvent::StderrLine { line } if line.ends_with(TRUNCATION_MARKER)))
            .expect("the runaway buffer was never flushed");
        let PromptParserEvent::StderrLine { line } = flushed else {
            unreachable!()
        };
        assert!(
            line.len() <= MAX_PARTIAL_BYTES + TRUNCATION_MARKER.len(),
            "the flushed line is not bounded: {} bytes",
            line.len()
        );
        // Output resumes cleanly once a newline arrives: the residue still in
        // the buffer is emitted as one ordinary line, terminated by the newline.
        let resumed = p.feed(" tail\n");
        assert!(
            matches!(resumed.as_slice(), [PromptParserEvent::StderrLine { line }] if line.ends_with(" tail")),
            "expected one stderr line ending in the tail, got {} events",
            resumed.len()
        );
    }

    #[test]
    fn the_partial_buffer_stays_under_the_cap() {
        // The bound itself, asserted on the state rather than on an event: this
        // is what makes the constant load-bearing rather than decorative.
        let mut p = PromptParser::new();
        for _ in 0..300 {
            p.feed(&"x".repeat(10_000));
            assert!(
                p.buf.len() <= MAX_PARTIAL_BYTES,
                "buffer grew past the cap: {} bytes",
                p.buf.len()
            );
        }
    }

    #[test]
    fn the_partial_buffer_fills_to_the_cap_before_it_flushes() {
        // The other half of the bound, and the half that pins the NUMBER: a
        // smaller cap flushes early and truncates output the user should have
        // seen whole. Asserting only "never grows past the cap" would pass with
        // the cap set to anything smaller, including zero.
        // The byte counts are LITERAL rather than derived from the constant. A
        // test written in terms of `MAX_PARTIAL_BYTES` scales with whatever the
        // constant is changed to and therefore cannot see the change at all.
        let mut p = PromptParser::new();
        let mut events = Vec::new();
        for _ in 0..90 {
            events.extend(p.feed(&"x".repeat(10_000))); // 900,000 bytes
        }
        assert_eq!(
            events,
            vec![],
            "output was flushed before the buffer reached the cap"
        );
        for _ in 0..12 {
            events.extend(p.feed(&"x".repeat(10_000))); // past 1,000,000
        }
        assert_eq!(events.len(), 1, "expected exactly one flush: {events:?}");
    }

    #[test]
    fn an_oversized_fake_block_bails_back_to_idle() {
        // A real prompt is capped at 10 KB by Deno itself. A tool faking the
        // start line and then streaming chrome-looking junk must not swallow
        // output forever.
        //
        // The assertion has to be that the CHROME starts surfacing, which is the
        // stated reason for the cap: "so ordinary output stops being swallowed
        // as block chrome". A trailing unprefixed line surfaces either way (in
        // the in_prompt state an unprefixed line is stderr too), so a test that
        // only checks the last event passes with the cap removed entirely.
        let mut p = PromptParser::new();
        assert_eq!(
            p.feed(&format!("{B}Deno requests read access to \"/x\"{R}.\n")),
            vec![]
        );
        let chrome = "┠─ chrome-looking line\n";
        let mut events = Vec::new();
        for _ in 0..1200 {
            events.extend(p.feed(chrome));
        }
        events.extend(p.feed("plain line after bail\n"));
        assert_eq!(
            events.last(),
            Some(&stderr("plain line after bail")),
            "the trailing line never surfaced at all"
        );
        let surfaced = events
            .iter()
            .filter(|e| matches!(e, PromptParserEvent::StderrLine { line } if line.contains("chrome-looking")))
            .count();
        assert!(
            surfaced > 0,
            "the parser never bailed: every chrome line was still being swallowed"
        );
        // And the bail happened where the cap says, rather than immediately: the
        // lines fed before the block passed 16,384 bytes were swallowed. The
        // count is literal rather than derived from the constant, so shrinking
        // the constant is visible here.
        assert!(
            surfaced < 1200 - 500,
            "the block bailed far too early: {surfaced} of 1200 chrome lines surfaced"
        );
    }

    #[test]
    fn a_block_just_under_the_cap_still_prompts() {
        // The positive control for the bail: without it, a cap of zero would
        // pass the test above. The byte count is LITERAL, because a filler
        // derived from MAX_BLOCK_BYTES scales with the constant and so cannot
        // see it change.
        let mut p = PromptParser::new();
        p.feed("┏ ⚠️  Deno requests read access to \"/x\".\n");
        let filler = "┠─ x\n"; // 9 bytes
        let mut events = Vec::new();
        for _ in 0..1_700 {
            // ~15,300 bytes, comfortably inside 16,384 and well outside 8,192.
            events.extend(p.feed(filler));
        }
        events.extend(p.feed("┗ Allow? [y/n/A] (y = yes, allow) > "));
        assert_eq!(events, vec![prompt("read", "/x", None)]);
    }

    #[test]
    fn an_osc_sequence_is_stripped_like_a_csi_one() {
        let mut p = PromptParser::new();
        assert_eq!(
            p.feed("\x1b]0;window title\x07plain\n"),
            vec![stderr("plain")]
        );
    }
}
