// How a CoreClient reaches its Core.
//
// There are two, and they differ in one fact with two consequences rather than
// in a list of switches: a remote Core is on the other end of a pinned TLS
// socket, and an embedded Core is inside this app with no socket at all.
//
// CoreClient stays transport-only: it does not know which core is ours, whether
// a process is up, or how a request crosses the boundary. The registry picks
// the transport when it builds the client (see cores.ts `select`).

import { type NetRequest, type NetResponse, type NetSocket, platform } from "../platform/index.ts";

export interface CoreTransport {
  fetch(req: NetRequest): Promise<NetResponse>;
  connectWebSocket(url: string, opts: { mode: "pin" | "webpki"; pin?: string }): Promise<NetSocket>;
  /** True when there is no socket: the Core runs inside this app and requests
   *  go straight into its router.
   *
   *  TWO things follow, and both are consequences of that one fact rather than
   *  independent settings:
   *
   *  - **No WS ticket is minted.** The wire URL exists to carry `?ticket=`, and
   *    the ticket is what the in-process identity replaces. Minting one anyway
   *    would send a credential nobody reads and make the connection depend on a
   *    route that has no part in it.
   *  - **The brisk reconnect profile.** Nothing is on a wire, so a re-open costs
   *    nothing and waiting 30s to retry would be pure delay.
   *
   *  What it does NOT change is the handling of `close`. A local `close` is
   *  always real (a superseded frame-bus pump stays silent), so it means this
   *  connection is genuinely gone and the answer is to tear down and re-open.
   *  `MAX_ATTEMPTS` stays intact rather than being bypassed, and it is
   *  REACHABLE: a Core that will not boot rejects the open, and enough
   *  rejections park the client. That is the correct outcome, since the
   *  alternative is retrying a Core that will not start. */
  readonly inProcess: boolean;
}

/** The default: pinned HTTP + WebSocket to a remote Core, terminated below the
 *  webview. Resolves `platform()` per call rather than at construction, so a
 *  client may be built before the platform is installed, exactly as before. */
export function netTransport(): CoreTransport {
  return {
    fetch: (req) => platform().net.fetch(req),
    connectWebSocket: (url, opts) => platform().net.connectWebSocket(url, opts),
    inProcess: false,
  };
}
