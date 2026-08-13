// Global test setup loaded by vitest before every test file. Keep this thin.
// Anything specific to one test should live in that test's beforeEach.
//
// IntersectionObserver and ResizeObserver are unimplemented in jsdom; the
// UI uses both, so stub them here. matchMedia is also unimplemented and is
// queried by responsive layout composables.

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/svelte";
import { afterEach } from "vitest";

// Cleanup mounted Svelte components between tests so the next test gets a
// fresh DOM. svelteTesting()'s built-in autoCleanup lives at a path Vite's
// loader can't resolve through Deno's node_modules; we wire it manually.
afterEach(() => cleanup());

// IntersectionObserver shape varies across DOM lib versions (newer adds
// `scrollMargin`). The `as unknown as ...` cast at install-time lets the
// stub satisfy whatever the host's lib.dom.d.ts expects without us having
// to chase every minor field.
class StubIntersectionObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  takeRecords(): unknown[] {
    return [];
  }
}

class StubResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

globalThis.IntersectionObserver =
  StubIntersectionObserver as unknown as typeof IntersectionObserver;
globalThis.ResizeObserver = StubResizeObserver as unknown as typeof ResizeObserver;

if (typeof globalThis.matchMedia !== "function") {
  // jsdom is missing matchMedia entirely; provide a no-op stub so layout
  // composables don't crash on import.
  globalThis.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
}
