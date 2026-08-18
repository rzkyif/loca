// `PairingApi` operates on an ALREADY-SELECTED core, so it must reach that core
// the way the rest of the client does. That is the whole content of this file:
// the UNPAIRED free functions below it (probe, PAKE) genuinely have no client
// and no transport yet, and stay on the network layer by design.

import { describe, expect, it } from "vitest";
import { CoreClient } from "./client";
import { localTransport } from "./local-transport";
import { PairingApi } from "./pairing";
import { type Platform, setPlatform } from "../platform/index";
import { fakeLocalCore } from "../../test/local-core-fake";

describe("PairingApi.mintCode", () => {
  it("goes through the selected client's transport, carrying the admin token", async () => {
    // Reaching for `platform().net` here would send a real network request to
    // whatever the selected core's base URL says, and for the Core inside this
    // app that URL is a sentinel with nothing behind it.
    const core = fakeLocalCore();
    setPlatform({
      localCore: core,
      net: {
        fetch: () => Promise.reject(new Error("the network must not be reached")),
      },
    } as unknown as Platform);

    const client = new CoreClient(
      { baseUrl: "https://local.loca.invalid", token: "", trustMode: "webpki" },
      undefined,
      localTransport(),
    );
    core.reply({ status: 200, bodyB64: btoa('{"code":"123456"}') });

    const res = await new PairingApi(client).mintCode("admin-token");

    expect(res).toEqual({ code: "123456" });
    const req = core.requests.at(-1);
    expect(req?.url).toBe("/api/v1/pairing/codes");
    expect(req?.method).toBe("POST");
    expect(req?.headers["X-Admin-Token"]).toBe("admin-token");
  });
});
