import type { Plugin } from "vite";
import { watch } from "node:fs";

// Dev-only Vite plugins, all in service of the same problem: the WebKit ESM
// TDZ race in SvelteKit's client bootstrap (sveltejs/kit#15287; root cause
// WebKit/WebKit#57827).
//
// In dev, Vite serves the app as native ESM. On a cold first load in the
// system WebKit behind Tauri's WKWebView, JSC can resolve a route node's
// dynamic `import()` before that module namespace's re-export bindings finish
// linking, so SvelteKit reading `branch_node.node.component` throws "Cannot
// access 'component' before initialization" and the route - and the whole UI -
// never renders. It is deterministic in the system WebKit (Playwright's WebKit
// build and V8 do not trip it), dev-only (Rollup bundles the graph for release
// so there is no native-ESM race), and has no upstream fix - the issue was
// closed stale, and client.js is byte-identical across the 2.50.1 -> 2.50.2
// boundary where the regression first appeared, so the trigger is the dev
// module-graph shape, not our code.

// `patchKitTdzRace` is the actual fix - it closes the race. The other two
// plugins/config knobs (restartOnRustChange below; optimizeDeps.include and
// the .svelte-kit/generated watch ignore in vite.config.ts) only shrink the
// race window; they are kept because they remain real dev wins on their own.
//
// The fix patches @sveltejs/kit's client `load_node`: after awaiting the node
// loader it touches `.component`; if that throws the transient TDZ it yields a
// macrotask and re-reads until the module finishes linking (~10-20 ticks on a
// cold graph). On engines without the bug the touch succeeds on the first try,
// so it is a no-op there. It warns if the target line stops matching (e.g. a
// kit upgrade) so a silently-broken boot surfaces immediately.
export const patchKitTdzRace = (): Plugin => {
  const TARGET = "const node = await loader();";
  const REPLACEMENT = [
    "let node = await loader();",
    "\tfor (let _tdz = 0; _tdz < 100; _tdz++) {",
    "\t\ttry { void node.component; break; }",
    "\t\tcatch { await new Promise((r) => setTimeout(r, 0)); node = await loader(); }",
    "\t}",
  ].join("\n");
  let warned = false;
  return {
    name: "loca-kit-tdz-race-fix",
    apply: "serve",
    enforce: "pre",
    transform(code, id) {
      const file = id.split("?")[0];
      if (!file.includes("@sveltejs/kit") || !file.endsWith("/runtime/client/client.js")) {
        return;
      }
      if (!code.includes(TARGET)) {
        if (!warned) {
          warned = true;
          this.warn(
            "kit TDZ-race patch did not match `load_node` - sveltejs/kit#15287 " +
              "may be fixed or refactored; verify the dev boot still renders in WebKit",
          );
        }
        return;
      }
      return { code: code.replace(TARGET, REPLACEMENT), map: null };
    },
  };
};

// When Tauri rebuilds Rust and relaunches the webview, the fresh Vite startup
// calls sync.all() which rewrites .svelte-kit/generated/ files; the watcher
// then streams those as HMR updates into the mid-bootstrap webview, which
// re-triggers the TDZ race above. Restarting Vite on a Rust/Cargo change lets
// the new webview hit a clean process (the .svelte-kit/generated watch ignore
// in vite.config.ts stops the spurious HMR from the regeneration itself).
export const restartOnRustChange = (): Plugin => ({
  name: "loca-restart-on-rust-change",
  configureServer(server) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onChange = (_: unknown, filename: string | Buffer | null) => {
      if (!filename) return;
      const f = String(filename);
      if (!f.endsWith(".rs") && !f.endsWith("Cargo.toml")) return;
      clearTimeout(timer);
      timer = setTimeout(() => {
        server.config.logger.info("[loca] rust change, restarting vite");
        server.restart();
      }, 100);
    };
    const watchers = [
      watch("src/tauri/src", { recursive: true }, onChange),
      watch("src/tauri/Cargo.toml", onChange),
    ];
    server.httpServer?.once("close", () => {
      clearTimeout(timer);
      for (const w of watchers) w.close();
    });
  },
});
