// SystemMessage is a thin wrapper over the shared ExpandableMessageView: it
// renders the "System Prompt" label and feeds the content. The expansion/body
// behavior lives in (and is tested against) ExpandableMessageView.

import { beforeEach, describe, expect, it } from "vitest";
import { render } from "@testing-library/svelte";
import SystemMessage from "./SystemMessage.svelte";
import { settingsState } from "$stores";

beforeEach(() => {
  settingsState.currentSettings["layout.alignment"] = "center";
});

describe("SystemMessage", () => {
  it("renders the 'System Prompt' label", () => {
    const { container } = render(SystemMessage, {
      props: { id: "sys-1", content: "You are a helpful AI assistant." },
    });
    expect(container.textContent).toContain("System Prompt");
  });

  it("starts collapsed (no body) with no expansion provider mounted", () => {
    const { container } = render(SystemMessage, {
      props: { id: "sys-2", content: "You are a helpful AI assistant." },
    });
    expect(container.querySelector(".whitespace-pre-wrap")).toBeNull();
  });
});
