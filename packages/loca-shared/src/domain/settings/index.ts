// Barrel for the shared settings schema. The schema + types + helpers all
// live in @loca/shared so client (renderer) and core (defaults / wire
// format) read from a single source.

export * from "./types.ts";
export * from "./engine.ts";
export * from "./quick-settings.ts";
// Re-export the group definitions so callers (server-side voice catalog,
// preset lookup, etc.) can introspect a specific group without walking
// SETTINGS_SCHEMA.
export { appearanceGroup } from "./groups/appearance.ts";
export { coresGroup } from "./groups/cores.ts";
export { dualModelGroup } from "./groups/dual-model.ts";
export { generalGroup } from "./groups/general.ts";
export { greetingsGroup } from "./groups/greetings.ts";
export { llmGroup } from "./groups/llm.ts";
export { memoriesGroup } from "./groups/memories.ts";
export { promptsGroup } from "./groups/prompts.ts";
export { scheduledPromptsGroup } from "./groups/scheduled-prompts.ts";
export { shortcutsGroup } from "./groups/shortcuts.ts";
export { snippetsGroup } from "./groups/snippets.ts";
export { sttGroup } from "./groups/stt.ts";
export { toolsGroup } from "./groups/tools.ts";
export { extensionsGroup } from "./groups/extensions.ts";
export { mcpGroup } from "./groups/mcp.ts";
export { ttsGroup } from "./groups/tts.ts";
export { usageGroup } from "./groups/usage.ts";
