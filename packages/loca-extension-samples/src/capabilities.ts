// Host-module samples: the capabilities gated by per-tool permissions in
// loca.json (llm, tts, memories). Each is a minimal call into the matching
// ctx module so authors can see the shape of a request and its result.

import type { ToolContext } from "./types.ts";
import { stringArg } from "./sample-data.ts";

export async function sampleLlm(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ text: string }> {
  const prompt = stringArg(args, "prompt", "Say hello in one short sentence.");
  const { text } = await ctx.llm.complete({ prompt });
  return { text };
}

export async function sampleTts(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ mime: string; sampleRate: number; bytes: number }> {
  const text = stringArg(args, "text", "Hello from the samples extension.");
  const audio = await ctx.tts.speak(text);
  // Report the decoded byte length rather than the audio itself.
  let bytes: number;
  try {
    bytes = atob(audio.dataB64).length;
  } catch {
    bytes = audio.dataB64.length;
  }
  return { mime: audio.mime, sampleRate: audio.sampleRate, bytes };
}

export async function sampleMemory(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ titles: string[]; wrote: string; content: string }> {
  const title = stringArg(args, "title", "Sample note");
  const content = stringArg(args, "content", "This memory was written by the samples extension.");

  const listing = await ctx.memories.list();
  const titles = listing.map((m) => m.title);

  await ctx.memories.write(title, content);
  const stored = await ctx.memories.get(title);

  ctx.setProgress(1, "Wrote and read back a memory", title);
  return { titles, wrote: title, content: stored.content };
}
