/**
 * What the currently-selected core can do, read from `GET /api/v1/health` on
 * connect.
 *
 * The point is that capability is a property of the CORE, not of the device
 * rendering this UI. Those are different questions and platform sniffing gets
 * the interesting case backwards: a phone paired to a desktop core must still
 * offer extensions, scheduled prompts and stdio MCP servers. Device concerns
 * (touch affordances, bottom sheets, soft-keyboard behaviour, window geometry)
 * stay on `ui.platform` and the settings schema's `desktopOnly`; these do not.
 *
 * Sits beside coreStatusState rather than with the stores wired in +page.svelte,
 * for the same reason it does: both are per-core state that must reset on a core
 * SWITCH, so both attach from +layout.svelte.
 */

import type { CoreCapabilities, HealthResponse } from "@loca/shared";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";

const log = getLogger("capabilities");

class CapabilitiesState {
  /** Null until the first health response for this core lands. */
  current = $state<CoreCapabilities | null>(null);

  private subs = new Subscriptions();
  private lastCoreId: string | null = null;

  attach(): void {
    this.subs.attach(() => {
      this.lastCoreId = cores().currentEntry()?.id ?? null;
      return [
        // The ONLY trigger. No poll, no timer, no boot fetch: this fires
        // because the user is already connected to this core, which is what
        // keeps it inside the repo's no-non-consented-network rule.
        cores().subscribeConnectionState((state) => {
          if (state === "connected") void this.refresh();
        }),
        cores().subscribe(() => {
          const id = cores().currentEntry()?.id ?? null;
          if (id === this.lastCoreId) return;
          // A different core answers differently, and keeping the previous
          // core's answer would gate this one's UI on the wrong facts. Back to
          // unknown until its own health response arrives.
          this.lastCoreId = id;
          this.current = null;
        }),
      ];
    });
  }

  detach(): void {
    this.subs.detach();
    this.lastCoreId = null;
    this.current = null;
  }

  /** Whether the connected core can do `flag`.
   *
   *  **Unknown means capable**, and the asymmetry is the whole argument. A
   *  false negative hides a working feature with no affordance to discover it,
   *  and the user cannot tell a hidden feature from a missing one. A false
   *  positive shows a control that either works or fails with a worded reason
   *  ("This device cannot run a local MCP server. Use a remote server
   *  instead."). The second is recoverable and the first is not.
   *
   *  It is also what keeps a core that predates negotiation working: it answers
   *  health with no `capabilities` block at all, and a pessimistic default
   *  would blank half its settings panel. */
  can(flag: keyof CoreCapabilities): boolean {
    return this.current?.[flag] ?? true;
  }

  private async refresh(): Promise<void> {
    const client = cores().currentClient();
    if (!client) return;
    const forCoreId = cores().currentEntry()?.id ?? null;
    try {
      // Health ignores credentials by design, so the bearer this sends is
      // harmless; using the ordinary GET keeps the transport surface unchanged.
      const body = await client.get<HealthResponse>("/api/v1/health");
      // The user may have switched cores while this was in flight. Applying it
      // would gate the NEW core's UI on the OLD core's capabilities, which is
      // exactly the bug this store exists to prevent, in miniature.
      if ((cores().currentEntry()?.id ?? null) !== forCoreId) return;
      this.current = body.capabilities ?? null;
    } catch (err) {
      // Left unknown rather than assumed incapable: a failed probe is a
      // transport problem, not evidence about the core. The next connect
      // retries it.
      log.warn("could not read core capabilities", err);
    }
  }
}

export const capabilitiesState = new CapabilitiesState();
