// Prompt-parser unit tests against transcripts recorded from deno 2.8.2 on a
// PTY (see the live-probe integration test for the re-recording path). The
// ANSI styling mirrors what the real prompter emits; chunked feeds cover
// frame boundaries landing mid-line and mid-escape-sequence.

import { assertEquals } from "@std/assert";
import { PromptParser, type PromptParserEvent } from "./prompt-parser.ts";

function collect(): { events: PromptParserEvent[]; parser: PromptParser } {
  const events: PromptParserEvent[] = [];
  const parser = new PromptParser((e) => events.push(e));
  return { events, parser };
}

const B = "\x1b[1m"; // bold
const R = "\x1b[0m"; // reset

// Faithful to the real block: styled segments, box-drawing prefixes, and the
// final Allow? line WITHOUT a trailing newline (Deno blocks on the answer).
function promptBlock(kind: string, resource: string, api?: string): string {
  let s = `┏ ⚠️  ${B}Deno requests ${R}${B}${kind} access to "${resource}"${R}${B}.${R}\n`;
  if (api) s += `┠─ Requested by \`${B}${api}${R}\` API.\n`;
  s +=
    "┠─ To see a stack trace for this prompt, set the DENO_TRACE_PERMISSIONS environmental variable.\n";
  s += `┠─ ${R}\x1b[3mLearn more at: ${R}\x1b[4m\x1b[36mhttps://docs.deno.com/go/--allow-${kind}${R}${R}\n`;
  s += `┠─ ${R}\x1b[3mRun again with --allow-${kind} to bypass this prompt.${R}\n`;
  s += `┗ ${B}Allow?${R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all ${kind} permissions) > `;
  return s;
}

function grantedLine(kind: string, resource: string): string {
  // Deno erases the block with cursor-up + erase-line sequences first.
  return `\x1b[1A\x1b[2K\x1b[1A\x1b[2K✅ ${B}Granted ${kind} access to "${resource}".${R}\n`;
}

function deniedLine(kind: string, resource: string): string {
  return `\x1b[1A\x1b[2K❌ ${B}Denied ${kind} access to "${resource}".${R}\n`;
}

Deno.test("parser: prompt then granted confirmation", () => {
  const { events, parser } = collect();
  parser.feed(promptBlock("read", "/etc/hosts", "Deno.readFileSync()"));
  assertEquals(events, [
    {
      kind: "prompt",
      permission: "read",
      resource: "/etc/hosts",
      apiName: "Deno.readFileSync()",
    },
  ]);
  parser.feed(grantedLine("read", "/etc/hosts"));
  assertEquals(events[1], { kind: "settled", granted: true });
});

Deno.test("parser: denied confirmation settles false", () => {
  const { events, parser } = collect();
  parser.feed(promptBlock("write", "/tmp/x.txt"));
  parser.feed(deniedLine("write", "/tmp/x.txt"));
  assertEquals(events[0], {
    kind: "prompt",
    permission: "write",
    resource: "/tmp/x.txt",
    apiName: undefined,
  });
  assertEquals(events[1], { kind: "settled", granted: false });
});

Deno.test("parser: blanket prompt without quoted resource", () => {
  const { events, parser } = collect();
  // e.g. Deno.env.toObject(): no `to "..."` segment.
  parser.feed(
    `┏ ⚠️  ${B}Deno requests env access${R}${B}.${R}\n` +
      `┗ ${B}Allow?${R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all env permissions) > `,
  );
  assertEquals(events, [
    {
      kind: "prompt",
      permission: "env",
      resource: "",
      apiName: undefined,
    },
  ]);
});

Deno.test("parser: stderr lines pass through around prompts", () => {
  const { events, parser } = collect();
  parser.feed("tool log line A\n");
  parser.feed(promptBlock("net", "example.com:443", "fetch()"));
  parser.feed(grantedLine("net", "example.com:443"));
  parser.feed("tool log line B\n");
  assertEquals(events[0], { kind: "stderr_line", line: "tool log line A" });
  assertEquals(events[1]?.kind, "prompt");
  assertEquals(events[2], { kind: "settled", granted: true });
  assertEquals(events[3], { kind: "stderr_line", line: "tool log line B" });
});

Deno.test("parser: sequential prompts in one stream", () => {
  const { events, parser } = collect();
  parser.feed(promptBlock("read", "/a"));
  parser.feed(grantedLine("read", "/a"));
  parser.feed(promptBlock("read", "/b"));
  parser.feed(deniedLine("read", "/b"));
  const kinds = events.map((e) => e.kind);
  assertEquals(kinds, ["prompt", "settled", "prompt", "settled"]);
  assertEquals((events[0] as { resource: string }).resource, "/a");
  assertEquals((events[2] as { resource: string }).resource, "/b");
  assertEquals((events[3] as { granted: boolean }).granted, false);
});

Deno.test("parser: reprinted Allow after garbled answer is swallowed", () => {
  const { events, parser } = collect();
  parser.feed(promptBlock("run", "ls"));
  // Unrecognized option: Deno erases + reprints the Allow line (now the
  // previous one completes with a newline).
  parser.feed(
    `\n┗ ${B}Unrecognized option. Allow?${R} [y/n/A] (y = yes, allow; n = no, deny; A = allow all run permissions) > `,
  );
  parser.feed(grantedLine("run", "ls"));
  assertEquals(
    events.map((e) => e.kind),
    ["prompt", "settled"],
  );
});

// A pseudoconsole (Windows ConPTY) reflects a screen buffer: it wraps output in
// cursor-hide/show, per-line absolute cursor moves + erase-line, and CRLF line
// endings. The parser strips all ANSI CSI and \r, so a prompt driven through a
// ConPTY must still yield the same prompt/settled events as the raw PTY. This is
// synthetic (real ConPTY output can only be captured on Windows), but it locks
// in that the VT chrome ConPTY adds is inert to the parser.
function conptyWrap(block: string): string {
  const HIDE = "\x1b[?25l";
  const SHOW = "\x1b[?25h";
  // Rewrite each line as ConPTY would paint it: move the cursor to the row,
  // erase the line, print the content, end with CRLF (except a no-newline tail,
  // which ConPTY leaves without an ending, matching the Allow? line).
  const lines = block.split("\n");
  const hasTail = lines[lines.length - 1] !== "";
  let out = HIDE;
  lines.forEach((line, i) => {
    if (line === "" && i === lines.length - 1) return;
    const last = i === lines.length - 1 || (i === lines.length - 2 && !hasTail);
    out += `\x1b[${i + 1};1H\x1b[K${line}`;
    if (!(last && hasTail)) out += "\r\n";
  });
  return out + SHOW;
}

Deno.test("parser: ConPTY-wrapped prompt still parses, resolves resource, and settles", () => {
  const { events, parser } = collect();
  parser.feed(conptyWrap(promptBlock("net", "example.com:443", "fetch()")));
  assertEquals(events, [
    {
      kind: "prompt",
      permission: "net",
      resource: "example.com:443",
      apiName: "fetch()",
    },
  ]);
  // The console echoes the typed answer while Deno reads it (observed live on
  // Windows: a bare "y"/"n" line between the prompt and the confirmation).
  // It is our own answer, not tool output, and must not leak as stderr.
  parser.feed("y\r\n");
  // The granted confirmation, likewise repainted by the pseudoconsole.
  parser.feed(conptyWrap(grantedLine("net", "example.com:443")));
  assertEquals(events[1], { kind: "settled", granted: true });
  assertEquals(events.length, 2);
});

Deno.test("parser: identical events under randomized chunking", () => {
  const transcript =
    "boot noise\n" +
    promptBlock("read", "/etc/hosts", "Deno.readFileSync()") +
    grantedLine("read", "/etc/hosts") +
    "mid noise\n" +
    promptBlock("net", "api.example.com:443", "fetch()") +
    deniedLine("net", "api.example.com:443") +
    "tail noise\n";
  const { events: reference, parser: refParser } = collect();
  refParser.feed(transcript);
  for (let round = 0; round < 25; round++) {
    const { events, parser } = collect();
    let i = 0;
    while (i < transcript.length) {
      const n = 1 + Math.floor(Math.random() * 37);
      parser.feed(transcript.slice(i, i + n));
      i += n;
    }
    assertEquals(events, reference, `chunked round ${round} diverged`);
  }
});

Deno.test("parser: runaway no-newline output is truncated, not buffered forever", () => {
  const { events, parser } = collect();
  // A tool streams >1 MB to stderr with no newline. The partial buffer must
  // not grow without bound; it gets flushed as a single truncated stderr line.
  for (let i = 0; i < 120; i++) parser.feed("x".repeat(10_000));
  const flush = events.find((e) => e.kind === "stderr_line" && e.line.endsWith("…[truncated]"));
  assertEquals(flush?.kind, "stderr_line");
  // Output resumes cleanly once a newline arrives.
  parser.feed(" tail\n");
  assertEquals(events.at(-1)?.kind, "stderr_line");
});

Deno.test("parser: oversized fake block bails back to idle", () => {
  const { events, parser } = collect();
  parser.feed(`${B}Deno requests read access to "/x"${R}.\n`);
  // A real prompt is capped at 10 KB by Deno itself; a tool faking the start
  // line and then streaming junk must not swallow output forever.
  const junk = "┠─ chrome-looking line\n";
  for (let i = 0; i < 1200; i++) parser.feed(junk);
  parser.feed("plain line after bail\n");
  assertEquals(events.at(-1), {
    kind: "stderr_line",
    line: "plain line after bail",
  });
});
