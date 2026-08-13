// Regression test for the scheduled-prompts manager: opening a card must reach
// the detail pane. The detail snapshots the opened prompt's schedule into local
// draft state; because the manager hands it the live store row (a Svelte $state
// Proxy), a structuredClone of that schedule throws DataCloneError and silently
// breaks the detail render, leaving cards "unclickable" and freshly-created
// prompts stuck behind a blank pane. The detail must use $state.snapshot.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/svelte";
import type { ScheduledPrompt } from "@loca/shared";

let store: ScheduledPrompt[] = [];

function makePrompt(title: string): ScheduledPrompt {
  return {
    id: `sp_${store.length + 1}`,
    ownerClientId: "c1",
    title,
    instruction: "Say hello.",
    schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
    runMissed: false,
    enabled: true,
    nextRunAtMs: 4102444800000,
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

vi.mock("$lib/core", () => ({
  cores: () => ({
    api: () => ({
      scheduledPrompts: {
        list: () => Promise.resolve([...store]),
        create: (draft: { title: string }) => {
          const p = makePrompt(draft.title);
          store.push(p);
          return Promise.resolve(p);
        },
        update: (id: string, patch: Record<string, unknown>) => {
          const p = store.find((x) => x.id === id)!;
          Object.assign(p, patch);
          return Promise.resolve(p);
        },
        delete: (id: string) => {
          store = store.filter((x) => x.id !== id);
          return Promise.resolve();
        },
        run: () => Promise.resolve({ sessionId: "s1" }),
      },
    }),
  }),
}));

import { scheduledPromptsState } from "$stores/scheduled-prompts.svelte";
import ScheduledPromptsField from "./ScheduledPromptsField.svelte";

describe("ScheduledPromptsField", () => {
  it("opens the detail pane when a card is clicked", async () => {
    store = [makePrompt("Alpha")];
    // Mirror attach()'s connect-time preload so the manager renders onto rows.
    scheduledPromptsState.prompts = [...store];

    const { getByLabelText, getByText } = render(ScheduledPromptsField);
    await waitFor(() => expect(getByLabelText("Open Alpha")).toBeTruthy());

    // Every card carries an enabled/disabled status chip (parity with the tools
    // and memories lists).
    expect(getByText("Enabled")).toBeTruthy();

    await fireEvent.click(getByLabelText("Open Alpha"));

    // The detail pane renders the editable title field; it never appears if the
    // schedule snapshot throws while the detail mounts.
    await waitFor(() => expect(getByLabelText("Scheduled prompt title")).toBeTruthy());
  });
});
