// Open a URL in the user's default browser. Uses macOS's `open`, Linux's
// `xdg-open`, or Windows's `rundll32 url.dll,FileProtocolHandler` depending
// on the host. (Windows deliberately avoids `cmd /c start`: cmd re-parses
// its command line, so a URL containing `&`/`|`/`^` could chain a second
// command. rundll32 receives the URL as a single CreateProcess argument with
// no shell in between.)

import type { ToolContext } from "./types.ts";

export async function open(args: { url?: string }, ctx: ToolContext): Promise<{ opened: string }> {
  const url = typeof args?.url === "string" ? args.url.trim() : "";
  if (!url) throw new Error("url is required");
  if (!/^https?:\/\//i.test(url)) {
    throw new Error("only http(s) URLs are allowed");
  }
  ctx.setProgress(0.5, "Opening browser", url);

  const [bin, ...prefix] = pickOpenCmd();
  const proc = new Deno.Command(bin, {
    args: [...prefix, url],
    stdout: "null",
    stderr: "piped",
    signal: ctx.signal,
  }).spawn();
  const { code, stderr } = await proc.output();
  if (code !== 0) {
    const msg = new TextDecoder().decode(stderr).trim();
    throw new Error(`open command exited ${code}: ${msg || "no stderr"}`);
  }

  return { opened: url };
}

function pickOpenCmd(): string[] {
  switch (Deno.build.os) {
    case "darwin":
      return ["open"];
    case "windows":
      // rundll32 hands the URL straight to the default protocol handler with
      // no shell parsing it; the prefix args precede the URL appended by the
      // caller.
      return ["rundll32", "url.dll,FileProtocolHandler"];
    default:
      return ["xdg-open"];
  }
}
