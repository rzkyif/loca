// Hermetic tests for the install subcommand dispatcher + bootstrap. Runs the
// `bootstrap` verb against a temp LOCA_CORE_HOME on the dev channel (which skips
// the network extension plant), so no service registration or network is
// touched. Service registration (launchd/systemd/Task Scheduler) is exercised by
// the end-to-end verification, not here.

import { assertEquals, assertMatch } from "@std/assert";
import { maybeRunInstallCommand } from "./cli.ts";
import { enableBehindProxy } from "./bootstrap.ts";
import { paths } from "../paths.ts";

async function withDevCoreHome(fn: (home: string) => Promise<void>): Promise<void> {
  const home = await Deno.makeTempDir({ prefix: "loca-install-test-" });
  const priorHome = Deno.env.get("LOCA_CORE_HOME");
  const priorChannel = Deno.env.get("LOCA_CHANNEL");
  Deno.env.set("LOCA_CORE_HOME", home);
  Deno.env.set("LOCA_CHANNEL", "dev");
  try {
    await fn(home);
  } finally {
    if (priorHome === undefined) Deno.env.delete("LOCA_CORE_HOME");
    else Deno.env.set("LOCA_CORE_HOME", priorHome);
    if (priorChannel === undefined) Deno.env.delete("LOCA_CHANNEL");
    else Deno.env.set("LOCA_CHANNEL", priorChannel);
    await Deno.remove(home, { recursive: true }).catch(() => {});
  }
}

Deno.test("dispatcher returns null for non-subcommands", async () => {
  assertEquals(await maybeRunInstallCommand([]), null);
  assertEquals(await maybeRunInstallCommand(["--watch"]), null);
  assertEquals(await maybeRunInstallCommand(["frobnicate"]), null);
});

Deno.test("bootstrap mints a 0600 admin token and is idempotent", async () => {
  await withDevCoreHome(async () => {
    assertEquals(await maybeRunInstallCommand(["bootstrap"]), 0);

    const token = await Deno.readTextFile(paths().adminTokenFile);
    // 16 random bytes as lowercase hex.
    assertMatch(token.trim(), /^[0-9a-f]{32}$/);

    if (Deno.build.os !== "windows") {
      const mode = (await Deno.stat(paths().adminTokenFile)).mode ?? 0;
      assertEquals(mode & 0o777, 0o600);
    }

    // Dev channel plants no built-in extension (it resolves from the codebase),
    // so bootstrap makes no network request and leaves the extensions dir bare.
    for await (const entry of Deno.readDir(paths().extensionsDir)) {
      throw new Error(`unexpected extensions dir entry on dev: ${entry.name}`);
    }

    // A second run keeps the same token (idempotent).
    assertEquals(await maybeRunInstallCommand(["bootstrap"]), 0);
    assertEquals(await Deno.readTextFile(paths().adminTokenFile), token);
  });
});

Deno.test("bootstrap --bind-all seeds settings.json", async () => {
  await withDevCoreHome(async () => {
    assertEquals(await maybeRunInstallCommand(["bootstrap", "--bind-all"]), 0);
    const settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.bindHost"], "0.0.0.0");
    assertEquals(settings["server.behindProxy"], undefined);
  });
});

Deno.test("bootstrap --behind-proxy seeds settings.json, composing with --bind-all", async () => {
  await withDevCoreHome(async () => {
    assertEquals(await maybeRunInstallCommand(["bootstrap", "--bind-all", "--behind-proxy"]), 0);
    const settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.bindHost"], "0.0.0.0");
    assertEquals(settings["server.behindProxy"], true);
  });
});

Deno.test("bootstrap leaves an existing settings.json untouched", async () => {
  await withDevCoreHome(async () => {
    await maybeRunInstallCommand(["bootstrap"]);
    await Deno.writeTextFile(paths().settingsFile, `{"server.bindHost":"127.0.0.1"}\n`);
    assertEquals(await maybeRunInstallCommand(["bootstrap", "--bind-all", "--behind-proxy"]), 0);
    const settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.bindHost"], "127.0.0.1");
    assertEquals(settings["server.behindProxy"], undefined);
  });
});

// The enable-behind-proxy verb also restarts the OS service, so it is exercised
// end to end, not here; this covers just the additive settings merge the "flip"
// step relies on (the verb's restart half is service.ts's restartService).
Deno.test("enableBehindProxy merges into an existing settings.json, keeping other keys", async () => {
  await withDevCoreHome(async () => {
    await maybeRunInstallCommand(["bootstrap"]);
    // Stand in for the settings the running core has already written post-boot.
    await Deno.writeTextFile(
      paths().settingsFile,
      JSON.stringify({ "server.bindHost": "0.0.0.0", "llm.model": "x" }),
    );
    await enableBehindProxy();
    const settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.behindProxy"], true);
    assertEquals(settings["server.bindHost"], "0.0.0.0");
    assertEquals(settings["llm.model"], "x");
  });
});

Deno.test("enableBehindProxy creates settings.json when absent and is idempotent", async () => {
  await withDevCoreHome(async () => {
    // bootstrap (no seed flags) creates the dir tree but no settings.json.
    await maybeRunInstallCommand(["bootstrap"]);
    await enableBehindProxy();
    let settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.behindProxy"], true);
    // A second run is a no-op.
    await enableBehindProxy();
    settings = JSON.parse(await Deno.readTextFile(paths().settingsFile));
    assertEquals(settings["server.behindProxy"], true);
  });
});
