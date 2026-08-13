// Memory tools over the host's `ctx.memories` module: list (titles +
// summaries the agent can browse when relevance misses), write (create or
// replace), edit (exact find/replace), read, and show (render the content
// as a markdown bubble). Write/edit return a `memory_diff` result and
// read a `memory_content` result; the client renders those kinds
// specially (diff view / markdown).

import type { ToolContext } from "./types.ts";

function stringArg(args: Record<string, unknown>, key: string, allowEmpty = false): string {
  const value = args[key];
  if (typeof value !== "string" || (!allowEmpty && value.trim().length === 0)) {
    throw new Error(`missing required string argument "${key}"`);
  }
  return value;
}

export async function writeMemory(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{
  kind: "memory_diff";
  title: string;
  before: string;
  after: string;
  created: boolean;
}> {
  const title = stringArg(args, "title");
  const content = stringArg(args, "content", true);
  const result = await ctx.memories.write(title, content);
  ctx.setProgress(1, result.created ? "Memory created" : "Memory replaced", result.title);
  return { kind: "memory_diff", ...result };
}

export async function editMemory(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ kind: "memory_diff"; title: string; before: string; after: string }> {
  const title = stringArg(args, "title");
  const find = stringArg(args, "find");
  const replace = stringArg(args, "replace", true);
  const result = await ctx.memories.edit(title, find, replace);
  ctx.setProgress(1, "Memory edited", result.title);
  return { kind: "memory_diff", ...result };
}

export async function listMemories(
  _args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ memories: { title: string; kind: "knowledge" | "skill"; summary?: string }[] }> {
  const memories = await ctx.memories.list();
  ctx.setProgress(1, "Listed memories", `${memories.length} found`);
  return {
    memories: memories.map((m) => ({ title: m.title, kind: m.kind, summary: m.summary })),
  };
}

export async function readMemory(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ kind: "memory_content"; title: string; content: string }> {
  const title = stringArg(args, "title");
  const memory = await ctx.memories.get(title);
  return { kind: "memory_content", ...memory };
}

export async function readSkillFile(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ kind: "memory_content"; title: string; content: string }> {
  const title = stringArg(args, "title");
  const name = stringArg(args, "name");
  const file = await ctx.memories.getFile(title, name);
  return {
    kind: "memory_content",
    title: `${file.title} / ${file.name}`,
    content: file.content,
  };
}

export async function showMemory(
  args: Record<string, unknown>,
  ctx: ToolContext,
): Promise<{ title: string; shown: boolean }> {
  const title = stringArg(args, "title");
  const memory = await ctx.memories.get(title);
  ctx.display.markdown(memory.content);
  return { title: memory.title, shown: true };
}
