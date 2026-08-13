import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SessionListView from "../components/session-list/SessionListView.svelte";

// Plain display data for the session list. The client derives the equivalent
// rows from its live session store (resolving each title, pre-rendering the
// conversation summary, flagging the current session, and arming the delete
// confirm); these are scripted stand-ins covering a populated list (with one
// active row and one row mid-delete-confirm), a single-item list, and the empty
// state.

export const sessionListSamples = {
  // A populated list: the second row is the current session (active border),
  // the third is mid-delete (armed confirm), the last has no messages yet.
  populated: {
    rows: [
      {
        id: "s-001",
        title: "Trip planning for Kyoto",
        summary: 'User: "What\'s the best time to visit?" Agent: "Spring or autumn."',
        active: false,
        confirmingDelete: false,
      },
      {
        id: "s-002",
        title: "Refactor the storage browser",
        summary: 'User: "Extract a pure View." Agent: "On it, mapping the tree now."',
        active: true,
        confirmingDelete: false,
      },
      {
        id: "s-003",
        title: "Grocery list",
        summary: 'User: "Add oat milk." Agent: "Added."',
        active: false,
        confirmingDelete: true,
      },
      {
        id: "s-004",
        title: "Tuesday 27 May, 14:32",
        summary: "",
        active: false,
        confirmingDelete: false,
      },
    ],
  },

  // A single session, the current one.
  single: {
    rows: [
      {
        id: "s-001",
        title: "First conversation",
        summary: 'User: "Hello." Agent: "Hi, how can I help?"',
        active: true,
        confirmingDelete: false,
      },
    ],
  },

  // A fresh install: no sessions yet, the empty-state bubble shows.
  empty: {
    rows: [],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SessionListView>>>;
