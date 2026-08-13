// Greeting trigger wrapper. The greetings.* settings are client-local, so the
// caller gates on greetings.enabled / greetings.runOn itself and only invokes
// this when a greeting should run, passing the session title + instruction it
// wants. The core mints the session (after a per-client dedup + model-ready
// wait) and the client navigates via the session.created frame.

import type { CoreClient } from "./client";

export interface GreetingRunInput {
  sessionTitle: string;
  instruction: string;
  /** Least time between greetings for this client, in ms. The core dedups on
   *  this value (floored server-side), so a rapid re-show can't spam sessions. */
  cooldownMs: number;
}

export class GreetingsApi {
  constructor(private readonly client: CoreClient) {}

  run(input: GreetingRunInput): Promise<{ ran: boolean; sessionId?: string }> {
    return this.client.post("/api/v1/greetings/run", input);
  }
}
