// The composer's askUser answer assembly for the kind-discriminated questions:
// diff and image commit from one action-button click, files (multiselect) and
// table collect through the Submit action, single-select choice auto-submits,
// and table answers come back as column-keyed records of the edited grid. The
// form logic lives here (the View just renders the drafts); the buttons the
// composer shows come from `actions`.

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AskUserQuestion } from "@loca/shared";
import { AskUser } from "./use-askuser.svelte";
import { extensionsState } from "$stores";

let respond: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  vi.restoreAllMocks();
  respond = vi.spyOn(extensionsState, "respondAskUser").mockImplementation(() => {});
});

function awaiting(questions: AskUserQuestion[]): AskUser {
  const ask = new AskUser();
  ask.sync({ callId: "call-1", requestId: "req-1", questions });
  return ask;
}

function clickAction(ask: AskUser, label: string): void {
  const button = ask.actions.find((b) => b.label === label);
  if (!button) throw new Error(`no action button "${label}"`);
  button.onClick();
}

// Mirrors the AskUserForm wrapper's auto-submit $effect for the no-explicit-
// submit kinds (single-select choice/files, diff, image).
function autoSubmit(ask: AskUser): void {
  if (!ask.requiresSubmit && ask.readyToSubmit) ask.submit();
}

describe("AskUser answer assembly", () => {
  it("diff: clicking Accept commits the verdict", () => {
    const ask = awaiting([
      { kind: "diff", question: "Apply this change?", before: "a\nb", after: "a\nc" },
    ]);
    clickAction(ask, "Accept");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", ["accept"]);
  });

  it("diff: clicking Reject commits the verdict", () => {
    const ask = awaiting([
      { kind: "diff", question: "Apply this change?", before: "a", after: "b" },
    ]);
    clickAction(ask, "Reject");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", ["reject"]);
  });

  it("files multiselect: picked paths submit via the Submit action", () => {
    const ask = awaiting([
      {
        kind: "files",
        question: "Which files?",
        entries: [
          { path: "/dl/a.txt", label: "a.txt" },
          { path: "/dl/b.txt", label: "b.txt" },
          { path: "/dl/c.txt", label: "c.txt" },
        ],
        multiselect: true,
      },
    ]);
    ask.togglePick(0, "/dl/a.txt", true);
    ask.togglePick(0, "/dl/c.txt", true);
    expect(respond).not.toHaveBeenCalled();
    clickAction(ask, "Submit");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", [["/dl/a.txt", "/dl/c.txt"]]);
  });

  it("image: clicking an action commits its value", () => {
    const ask = awaiting([
      {
        kind: "image",
        question: "Keep this image?",
        dataB64: "aGk=",
        mime: "image/png",
        actions: [
          { label: "Keep", value: "keep" },
          { label: "Discard", value: "discard" },
        ],
      },
    ]);
    clickAction(ask, "Discard");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", ["discard"]);
  });

  it("table: edited rows submit as column-keyed records", () => {
    const ask = awaiting([
      {
        kind: "table",
        question: "Check the rows.",
        columns: ["item", "amount"],
        rows: [
          ["coffee", "4.50"],
          ["lunch", "12.00"],
        ],
      },
    ]);
    expect(ask.drafts[0].rows).toEqual([
      ["coffee", "4.50"],
      ["lunch", "12.00"],
    ]);
    ask.setCell(0, 0, 1, "5.00");
    clickAction(ask, "Submit");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", [
      [
        { item: "coffee", amount: "5.00" },
        { item: "lunch", amount: "12.00" },
      ],
    ]);
  });

  it("table: row add and remove reshape the submitted records", () => {
    const ask = awaiting([
      { kind: "table", question: "Check the rows.", columns: ["name"], rows: [["amy"], ["bo"]] },
    ]);
    ask.removeRow(0, 0);
    ask.addRow(0, 1);
    expect(ask.drafts[0].rows).toEqual([["bo"], [""]]);
    ask.setCell(0, 1, 0, "cy");
    clickAction(ask, "Submit");
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", [[{ name: "bo" }, { name: "cy" }]]);
  });

  it("legacy choice questions auto-submit a single-select pick", () => {
    const ask = awaiting([
      {
        question: "Pick a color.",
        options: [
          { label: "Red", value: "red" },
          { label: "Blue", value: "blue" },
        ],
      },
    ]);
    expect(ask.actions).toEqual([]);
    ask.togglePick(0, "blue", false);
    autoSubmit(ask);
    expect(respond).toHaveBeenCalledWith("call-1", "req-1", ["blue"]);
  });
});
