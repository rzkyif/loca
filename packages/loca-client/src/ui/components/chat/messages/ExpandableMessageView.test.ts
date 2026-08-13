// ExpandableMessageView is the shared shell for the small core-authored
// bubbles (system/automated/display). With no UiContext provider mounted it
// uses DEFAULT_UI_CONTEXT, so `defaultExpanded` controls the initial state and
// the body renders inline when expanded. (In the app the layout provider backs
// expansion with the live registry; that wiring is covered in-app.)

import { beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";
import { settingsState } from "$stores";

beforeEach(() => {
  settingsState.currentSettings["layout.alignment"] = "center";
});

describe("ExpandableMessageView", () => {
  it("renders the title and stays collapsed by default", () => {
    const { container } = render(ExpandableMessageView, {
      props: {
        title: "System Prompt",
        text: "You are a helpful AI assistant.",
      },
    });
    expect(container.textContent).toContain("System Prompt");
    expect(container.querySelector(".whitespace-pre-wrap")).toBeNull();
  });

  it("renders the text body when defaultExpanded", () => {
    const { container } = render(ExpandableMessageView, {
      props: {
        title: "System Prompt",
        text: "You are a helpful AI assistant.",
        defaultExpanded: true,
      },
    });
    expect(container.textContent).toContain("helpful AI assistant");
    expect(container.querySelector(".whitespace-pre-wrap")).toBeTruthy();
  });

  it("preserves whitespace + newlines in the expanded body", () => {
    const { container } = render(ExpandableMessageView, {
      props: {
        title: "System Prompt",
        text: "line 1\nline 2\n  indented",
        defaultExpanded: true,
      },
    });
    const body = container.querySelector(".whitespace-pre-wrap");
    expect(body).toBeTruthy();
    expect(body!.textContent).toBe("line 1\nline 2\n  indented");
  });

  it("toggles via the disclosure with no id (local fallback)", async () => {
    // With no `id`, the function binding falls back to local state, so clicking
    // the disclosure flips expansion without a mounted registry. Assert on the
    // disclosure title (flips synchronously with state) and the body mounting on
    // expand; the collapse animation keeps the body mounted, so we don't assert
    // its removal here.
    const { container, getByRole } = render(ExpandableMessageView, {
      props: {
        title: "System Prompt",
        text: "You are a helpful AI assistant.",
      },
    });
    const button = getByRole("button");
    expect(button.getAttribute("title")).toBe("Expand");
    expect(container.querySelector(".whitespace-pre-wrap")).toBeNull();
    await fireEvent.click(button);
    expect(button.getAttribute("title")).toBe("Collapse");
    expect(container.querySelector(".whitespace-pre-wrap")).toBeTruthy();
    await fireEvent.click(button);
    expect(button.getAttribute("title")).toBe("Expand");
  });
});
