// Stream parser for Deno's interactive permission prompt as it appears on a
// tool worker's PTY (see worker-handle.ts and the loca-core-ptyhost helper).
// Pure state machine over text chunks; no I/O, so it is unit-testable against
// recorded transcripts.
//
// A prompt block looks like (ANSI styling stripped):
//
//   ┏ ⚠️  Deno requests read access to "/etc/hosts".
//   ┠─ Requested by `Deno.readFileSync()` API.
//   ┠─ To see a stack trace for this prompt, set the DENO_TRACE_PERMISSIONS ...
//   ┠─ Learn more at: https://docs.deno.com/go/--allow-read
//   ┠─ Run again with --allow-read to bypass this prompt.
//   ┗ Allow? [y/n/A] (y = yes, allow; ...) >
//
// The final line has NO trailing newline (Deno blocks reading the answer), so
// prompt completion is detected on the partial line buffer. After the answer
// is accepted Deno erases the block (ANSI cursor moves) and prints one
// confirmation line: `✅ Granted ...` or `❌ Denied ...`.
//
// Anything that is not prompt grammar is forwarded as a stderr line, exactly
// like the piped-stderr path: the same PTY carries the tool's own stderr.
// The wording is not a stable Deno API; the regexes anchor on the substrings
// that have survived every Deno 2.x release ("Deno requests", "Allow?",
// "Granted"/"Denied"), the bundled deno version is pinned on every channel,
// and the live-probe test re-validates the format against the real binary.

export type PromptParserEvent =
  | { kind: "stderr_line"; line: string }
  // `permission` is the raw kind string from the prompt (usually one of the
  // seven loca.json kinds, but e.g. "import" can appear); the matcher maps
  // it. `resource` is empty for blanket requests without a quoted resource.
  | { kind: "prompt"; permission: string; resource: string; apiName?: string }
  | { kind: "settled"; granted: boolean };

// Deno caps prompt content at 10 KB; anything bigger is not a real prompt.
const MAX_BLOCK_BYTES = 16_384;

// Bound the partial (pre-newline) line buffer. A tool sharing the PTY can write
// an endless stream to stderr with no newline; without this cap `buf` would
// grow without limit and exhaust core's memory. Mirrors the pipe-mode stderr
// cap in worker-handle.ts. A real prompt line is far below this (its block is
// already capped at MAX_BLOCK_BYTES), so this never truncates genuine prompts.
const MAX_PARTIAL_BYTES = 1_000_000;

// oxlint-disable-next-line no-control-regex
const ANSI_RE = /\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))/g;

const PROMPT_START_RE = /Deno requests (\w+) access(?: to "([^"]*)")?/;
const API_LINE_RE = /Requested by `(.+)` API/;
const ALLOW_TAIL_RE = /Allow\?.*>\s*$/;
const BLOCK_PREFIX_RE = /^[┏┠┃┗├└]/;

function stripAnsi(text: string): string {
  return text.replaceAll(ANSI_RE, "").replaceAll("\r", "");
}

export class PromptParser {
  private buf = "";
  private state: "idle" | "in_prompt" | "awaiting" = "idle";
  private permission = "";
  private resource = "";
  private apiName: string | undefined;
  private blockBytes = 0;

  constructor(private readonly onEvent: (event: PromptParserEvent) => void) {}

  feed(text: string): void {
    this.buf += text;
    const lines = this.buf.split("\n");
    this.buf = lines.pop() ?? "";
    for (const line of lines) this.handleLine(stripAnsi(line));
    // The Allow? line never gets a newline while Deno waits for the answer,
    // so check the partial buffer. ANSI sequences split across chunks make
    // the strip incomplete at worst; the next chunk completes them.
    if (this.state === "in_prompt") {
      this.blockBytes += text.length;
      if (this.blockBytes > MAX_BLOCK_BYTES) {
        // Not a real prompt (Deno caps blocks well below this); bail so
        // ordinary output stops being swallowed as block lines.
        this.state = "idle";
        return;
      }
      const partial = stripAnsi(this.buf);
      if (ALLOW_TAIL_RE.test(partial)) {
        this.state = "awaiting";
        this.onEvent({
          kind: "prompt",
          permission: this.permission,
          resource: this.resource,
          apiName: this.apiName,
        });
      }
    }
    // Runaway no-newline output (idle or awaiting state): flush a truncated
    // stderr line and drop the buffer so memory stays bounded. The in_prompt
    // state can't reach here with a large buffer (MAX_BLOCK_BYTES bails it to
    // idle first), so this only ever fires on non-prompt chrome/output.
    if (this.buf.length > MAX_PARTIAL_BYTES) {
      const flushed = stripAnsi(this.buf);
      this.buf = "";
      if (flushed.trim()) {
        this.onEvent({
          kind: "stderr_line",
          line: flushed.slice(0, MAX_PARTIAL_BYTES) + " …[truncated]",
        });
      }
    }
  }

  /** Settle on a Granted/Denied confirmation in `line`; true when settled. */
  private checkSettle(line: string): boolean {
    if (line.includes("✅") && line.includes("Granted")) {
      this.state = "idle";
      this.onEvent({ kind: "settled", granted: true });
      return true;
    }
    if (line.includes("❌") && line.includes("Denied")) {
      this.state = "idle";
      this.onEvent({ kind: "settled", granted: false });
      return true;
    }
    return false;
  }

  private handleLine(line: string): void {
    if (!line.trim()) return;
    switch (this.state) {
      case "idle": {
        const start = line.match(PROMPT_START_RE);
        if (start) {
          this.state = "in_prompt";
          this.permission = start[1];
          this.resource = start[2] ?? "";
          this.apiName = undefined;
          this.blockBytes = line.length;
          return;
        }
        this.onEvent({ kind: "stderr_line", line });
        return;
      }
      case "in_prompt": {
        const api = line.match(API_LINE_RE);
        if (api) {
          this.apiName = api[1];
          return;
        }
        // The Allow? line normally surfaces via the partial-buffer check in
        // feed() (it carries no newline while Deno waits), but buffering can
        // also deliver it as a completed line, possibly with the
        // confirmation glued on after the "> ". Emit the prompt here too,
        // then let the same line settle if it already carries the verdict.
        if (line.includes("Allow?")) {
          this.state = "awaiting";
          this.onEvent({
            kind: "prompt",
            permission: this.permission,
            resource: this.resource,
            apiName: this.apiName,
          });
          this.checkSettle(line);
          return;
        }
        // Box-drawing lines are prompt chrome (stack frames, doc links).
        // The tool's JS thread is blocked during a prompt, so genuine tool
        // stderr cannot interleave here; anything unprefixed is Deno's own
        // (e.g. a clear_stdin error) and still surfaces as stderr.
        if (BLOCK_PREFIX_RE.test(line)) return;
        this.onEvent({ kind: "stderr_line", line });
        return;
      }
      case "awaiting": {
        if (this.checkSettle(line)) return;
        // The Allow? line completes (gains its newline) once the answer is
        // read, possibly reprinted after a flushed/garbled answer; swallow
        // it and the rest of the block chrome.
        if (line.includes("Allow?") || BLOCK_PREFIX_RE.test(line)) return;
        // A console (ConPTY) echoes the typed answer while Deno reads it; the
        // tool's thread is still blocked, so a bare y/n here can only be our
        // own answer bouncing back, never tool output.
        if (/^[yn]$/i.test(line.trim())) return;
        this.onEvent({ kind: "stderr_line", line });
        return;
      }
    }
  }
}
