// Tier 1: core pairing flow. A fresh install locks to the pairing wizard; a
// completed pairing (real CPace PAKE over the core's self-signed TLS, cert-pin
// channel-bound) lands the app in chat, connected.
import { afterEach, expect, test } from "vitest";
import { page } from "vitest/browser";
import { launchApp, type AppHandle } from "../../harness/app.ts";

let app: AppHandle | undefined;
afterEach(async () => {
  await app?.dispose();
  app = undefined;
});

test("a fresh install locks to the pairing wizard", async () => {
  app = await launchApp({ scenario: "fresh" });
  await app.nav.expectMode("newCore");
  // No core paired => the chat composer is not present.
  await expect.element(page.getByTestId("composer-input")).not.toBeInTheDocument();
});

test("an installer-provided core auto-pairs on first boot into quick settings", async () => {
  // The core presents as installed by the native installer (admin token on
  // disk); the wizard silently pairs with it - real mint + CPace PAKE - and the
  // first-ever pairing lands on Quick Settings, never showing a form.
  app = await launchApp({ scenario: "installer" });
  await app.nav.expectMode("quickSettings");
});

test("completing the real PAKE pairing lands in chat, connected", async () => {
  // The "paired" scenario runs the real mintCode + pairWithCode flow against the
  // spawned core (full CPace handshake, real SPKI pin), then the app boots and
  // restoreSelected() connects.
  app = await launchApp({ scenario: "paired" });
  await app.nav.expectMode("chat");
  await expect.element(page.getByTestId("composer-input")).toBeEnabled({ timeout: 20_000 });
});
