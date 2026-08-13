// Vitest shim for SvelteKit's `$app/environment` virtual module. We don't
// run SvelteKit under vitest; jsdom counts as a browser-like environment
// for the purposes of state-init guards.

export const browser = true;
export const dev = true;
export const building = false;
export const version = "test";
