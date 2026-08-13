// ErrorMessage maps the persisted "<errorType>\n<detail>" wire format
// into a user-readable bubble. Verifies the error-type → message lookup
// table and the optional mono detail card (ErrorDetailView) for the detail.

import { beforeEach, describe, expect, it } from "vitest";
import { render } from "@testing-library/svelte";
import ErrorMessage from "./ErrorMessage.svelte";
import { settingsState } from "$stores";

// settingsState is a module-level singleton; force a known alignment so the
// Bubble subcomponent doesn't surface a settings-load race.
beforeEach(() => {
  settingsState.currentSettings["layout.alignment"] = "center";
});

describe("ErrorMessage", () => {
  it("renders a human-readable message for a known error type", () => {
    const { container } = render(ErrorMessage, {
      props: { content: "rate_limit_error" },
    });
    expect(container.textContent).toContain("Rate limit exceeded");
  });

  it("renders the detail text inside a mono card when provided", () => {
    const { container } = render(ErrorMessage, {
      props: { content: "server_error\nTraceback ...\n  at handler:42" },
    });
    expect(container.textContent).toContain("Server error");
    const detail = container.querySelector("pre");
    expect(detail).toBeTruthy();
    expect(detail!.textContent).toContain("Traceback");
    expect(detail!.textContent).toContain("handler:42");
  });

  it("falls back to the generic unexpected-error message for unknown types", () => {
    const { container } = render(ErrorMessage, {
      props: { content: "some_made_up_error" },
    });
    expect(container.textContent).toContain("An unexpected error occurred");
  });

  it("does not render the mono card when no detail line is present", () => {
    const { container } = render(ErrorMessage, {
      props: { content: "authentication_error" },
    });
    expect(container.textContent).toContain("Authentication failed");
    expect(container.querySelector("pre")).toBeNull();
  });

  it("handles a MessageContent array with a single text part", () => {
    const { container } = render(ErrorMessage, {
      props: {
        content: [{ type: "text", text: "context_length_exceeded_error" }],
      },
    });
    expect(container.textContent).toContain("Conversation is too long");
  });
});
