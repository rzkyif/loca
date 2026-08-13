// Hello-world spec: launches the debug Tauri binary, confirms the main window
// appears, and exits. The point of this spec is to validate the harness
// (tauri-driver + WDIO + Mocha) on the developer's machine. If it passes,
// add real E2E coverage; if it fails, document the platform limitation in
// tests/e2e/tauri-driver/README.md and don't invest further until upstream
// fixes land.

describe("Loca: app boots", () => {
  it("renders the main window", async () => {
    // wdio injects $ and browser globally under Mocha. The TS compiler
    // doesn't see them; we cast where needed.
    // deno-lint-ignore no-explicit-any
    const $$ = (globalThis as any).$;
    // deno-lint-ignore no-explicit-any
    const browser = (globalThis as any).browser;
    await browser.pause(500);
    const body = await $$("body");
    expect(await body.isExisting()).toBe(true);
  });
});
