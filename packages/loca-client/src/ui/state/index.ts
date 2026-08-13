/**
 * Re-exports for every reactive state module in this folder so other code
 * can import any state from a single path.
 */

export * from "./servers.svelte";
export * from "./settings.svelte";
export * from "./sessions.svelte";
export * from "./view.svelte";
export * from "./streaming.svelte";
export * from "./core-status.svelte";
export * from "./messages.svelte";
export * from "./confirm.svelte";
export * from "./password-prompt.svelte";
export * from "./permissions.svelte";
export * from "./schedule-confirm.svelte";
export * from "./deletions.svelte";
export * from "./color-picker.svelte";
export * from "./snippets.svelte";
export * from "./memories.svelte";
export * from "./scheduled-prompts.svelte";
export * from "./extensions.svelte";
export * from "./mcp.svelte";
export * from "./expansion.svelte";
export * from "./downloads.svelte";
export * from "./update.svelte";
export * from "./model-recommend.svelte";
// tts.svelte is not re-exported here. Consumers that need it import it
// directly from "$stores/tts.svelte".

// Side-effect import: wires settingsState.onChange listeners.
import "./settings-effects";
// Side-effect import: wires the messages<->streaming and sessions<->streaming
// store ports (kept out of the store modules to avoid re-forming the cycles).
import "./store-wiring";
