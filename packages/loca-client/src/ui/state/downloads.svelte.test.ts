// The single-source-of-truth invariant: every gate the UI reads (the chat
// block, the settings button, the auto-popup) derives from the authoritative
// `missing` set, NOT the transfer queue. This is what makes the chat view and
// the settings view unable to disagree about whether downloads are pending.

import { describe, expect, it } from "vitest";
import type { RequiredFile } from "@loca/shared";
import { downloadsState } from "./downloads.svelte";

function binary(error?: string): RequiredFile {
  return { source: "binary:llama-server", type: "binary", group: "binary", present: false, error };
}

describe("downloadsState gate derivations", () => {
  it("all gates key off `missing`, never the queue", () => {
    // No missing files: nothing pending anywhere, whatever the queue holds.
    downloadsState.missing = [];
    downloadsState.approvedSources = new Set();
    expect(downloadsState.hasPending).toBe(false);
    expect(downloadsState.needsApproval).toBe(false);
    expect(downloadsState.installing).toBe(false);
    expect(downloadsState.failed.length).toBe(0);

    // A missing, unapproved binary: chat gates (hasPending) and the button is in
    // approval mode (needsApproval) - the same `missing`, so they agree.
    downloadsState.missing = [binary()];
    downloadsState.approvedSources = new Set();
    expect(downloadsState.hasPending).toBe(true);
    expect(downloadsState.needsApproval).toBe(true);
    expect(downloadsState.installing).toBe(false);

    // Once approved it flips to "installing" (in progress) while still gating.
    downloadsState.approvedSources = new Set(["binary:llama-server"]);
    expect(downloadsState.hasPending).toBe(true);
    expect(downloadsState.needsApproval).toBe(false);
    expect(downloadsState.installing).toBe(true);
    expect(downloadsState.failed.length).toBe(0);

    // A recorded error makes it a retryable failure, not a phantom "installing".
    downloadsState.missing = [binary("could not resolve upstream")];
    expect(downloadsState.hasPending).toBe(true);
    expect(downloadsState.failed.length).toBe(1);
    expect(downloadsState.installing).toBe(false);
  });
});
