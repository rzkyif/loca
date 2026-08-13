// Tier 2: greetings. With greetings enabled for every start, the app's boot
// (which reports the launch to core's greeting trigger on connect) opens an
// automated greeting session.
import { afterEach, expect, test, vi } from "vitest";
import { launchApp, type AppHandle } from "../../harness/app.ts";
import { sessionsState } from "@client/state/sessions.svelte";
import { cores } from "@client/lib/core/cores.ts";
import type { AssistantMessage } from "@loca/shared";

let app: AppHandle | undefined;
afterEach(async () => {
  await app?.dispose();
  app = undefined;
});

test("an enabled greeting opens an automated session with the greeting reply on launch", async () => {
  app = await launchApp({
    scenario: "paired",
    llm: { kind: "text", text: "Hello and welcome!" },
    // greetings.* are client-local (destination: client-on-client): the client
    // gates on them and only POSTs /greetings/run when one should fire, so they
    // must be seeded into the client's local settings, not the core's.
    clientSettings: {
      "greetings.enabled": true,
      "greetings.runOn": "every_start",
      "greetings.instruction": "Greet the user.",
    },
  });
  await app.chat.waitReady();

  // The boot greeting fires in the background; the session.created frame adds a
  // greeting-titled session, and the automated turn persists the assistant
  // reply. Assert on the reply (not just a matching title) so a stale or empty
  // session can't pass for a greeting that actually ran.
  await vi.waitFor(
    async () => {
      await sessionsState.loadList();
      const greeting = sessionsState.list.find((s) => /greeting/i.test(s.title));
      expect(greeting, "a greeting session was created on launch").toBeTruthy();

      const full = await cores().api().sessions.get(greeting!.id);
      const reply = full.messages.find((m): m is AssistantMessage => m.role === "assistant");
      expect(reply, "the greeting produced an assistant reply").toBeTruthy();
      expect(reply!.content).toContain("Hello and welcome!");
    },
    { timeout: 20_000, interval: 500 },
  );
});
