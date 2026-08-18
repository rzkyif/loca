// The transport for the Core inside this app. There is no socket to drive, so
// the platform boundary is faked (test/local-core-fake.ts) and the shipped
// localTransport() runs against it.

import { beforeEach, describe, expect, it } from "vitest";
import { type Platform, setPlatform } from "../platform/index";
import { fakeLocalCore, type FakeLocalCore } from "../../test/local-core-fake";
import { localTransport } from "./local-transport";

let core: FakeLocalCore;

function install(localCore: FakeLocalCore | null): void {
  setPlatform({ localCore } as unknown as Platform);
}

beforeEach(() => {
  core = fakeLocalCore();
  install(core);
});

const decode = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);

describe("localTransport fetch", () => {
  it("strips the origin so a concatenated base URL reaches the core as a path", async () => {
    await localTransport().fetch({
      url: "https://local.loca.invalid/api/v1/sessions?limit=20",
      mode: "webpki",
    });
    expect(core.requests[0].url).toBe("/api/v1/sessions?limit=20");
  });

  it("strips the origin off an absolute attachment URL too", async () => {
    // The path stored on a MessagePart is `<baseUrl>/api/v1/sessions/.../attachments/x`,
    // and it is fetched whole rather than as a path. It must resolve in process
    // like any other request.
    await localTransport().fetch({
      url: "https://local.loca.invalid/api/v1/sessions/s1/attachments/a1",
      mode: "webpki",
    });
    expect(core.requests[0].url).toBe("/api/v1/sessions/s1/attachments/a1");
  });

  it("passes a bare path through unchanged", async () => {
    await localTransport().fetch({ url: "/api/v1/health", mode: "webpki" });
    expect(core.requests[0].url).toBe("/api/v1/health");
  });

  it("defaults the method to GET and sends no body", async () => {
    await localTransport().fetch({ url: "/api/v1/health", mode: "webpki" });
    expect(core.requests[0].method).toBe("GET");
    expect(core.requests[0].bodyB64).toBeNull();
    expect(core.requests[0].headers).toEqual({});
  });

  it("passes the caller's headers through", async () => {
    // The bearer and the content type ride here. Dropping them would leave every
    // authenticated route answering 401 through a transport that looks fine.
    await localTransport().fetch({
      url: "/api/v1/sessions",
      method: "POST",
      headers: { Authorization: "Bearer t", "Content-Type": "application/json" },
      mode: "webpki",
    });
    expect(core.requests[0].headers).toEqual({
      Authorization: "Bearer t",
      "Content-Type": "application/json",
    });
  });

  it("round-trips a string body and a byte body", async () => {
    const t = localTransport();
    await t.fetch({ url: "/a", method: "POST", body: '{"k":"v"}', mode: "webpki" });
    await t.fetch({
      url: "/b",
      method: "POST",
      body: new Uint8Array([0, 1, 254, 255]),
      mode: "webpki",
    });
    expect(atob(core.requests[0].bodyB64 ?? "")).toBe('{"k":"v"}');
    const bytes = atob(core.requests[1].bodyB64 ?? "");
    expect([...bytes].map((c) => c.charCodeAt(0))).toEqual([0, 1, 254, 255]);
  });

  it("decodes the reply body back to bytes", async () => {
    core.reply({ status: 201, bodyB64: btoa('{"id":"x"}') });
    const res = await localTransport().fetch({ url: "/a", mode: "webpki" });
    expect(res.status).toBe(201);
    expect(decode(res.body)).toBe('{"id":"x"}');
  });

  it("resolves an HTTP failure rather than rejecting, carrying the envelope", async () => {
    core.reply({ status: 401, bodyB64: btoa('{"error":{"code":"missing_token"}}') });
    const res = await localTransport().fetch({ url: "/a", mode: "webpki" });
    expect(res.status).toBe(401);
    expect(decode(res.body)).toContain("missing_token");
  });

  it("reports no captured pin: there is no certificate in process", async () => {
    const res = await localTransport().fetch({ url: "/a", mode: "webpki" });
    expect(res.capturedPin).toBeUndefined();
  });

  it("fails loudly on a build with no Core inside it", async () => {
    install(null);
    await expect(localTransport().fetch({ url: "/a", mode: "webpki" })).rejects.toThrow(
      /no Core inside/,
    );
  });
});

describe("localTransport websocket", () => {
  it("registers its listeners before wsOpen, so an open fired from inside it survives", async () => {
    // The real boundary emits `open` before any seed frame. A transport that
    // subscribed after invoking wsOpen would drop it and sit in "connecting".
    core.onNextOpen(() => core.emitOpen());
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    let opened = false;
    sock.onOpen(() => {
      opened = true;
    });
    expect(opened).toBe(true);
  });

  it("buffers frames that arrive before onMessage is attached", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    core.emitMessage("first");
    core.emitMessage("second");
    const got: string[] = [];
    sock.onMessage((d) => got.push(d));
    expect(got).toEqual(["first", "second"]);
  });

  it("uses a fresh id per connection", async () => {
    const t = localTransport();
    await t.connectWebSocket("", { mode: "webpki" });
    await t.connectWebSocket("", { mode: "webpki" });
    expect(core.openedIds).toHaveLength(2);
    expect(core.openedIds[0]).not.toBe(core.openedIds[1]);
  });

  it("sends and closes against its own id", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    sock.send("frame");
    sock.close();
    const id = core.openedIds[0];
    expect(core.sent).toEqual([[id, "frame"]]);
    expect(core.closedIds).toEqual([id]);
  });

  it("detaches on close, so a later event reaches nothing", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    let messages = 0;
    sock.onMessage(() => messages++);
    sock.close();
    core.emitMessage("after");
    expect(messages).toBe(0);
  });

  it("detaches when the core closes the connection, so listeners do not pile up per reconnect", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    let closes = 0;
    let messages = 0;
    sock.onClose(() => closes++);
    sock.onMessage(() => messages++);
    core.emitClose();
    core.emitMessage("after");
    expect(closes).toBe(1);
    expect(messages).toBe(0);
  });

  it("reports a close that arrived before onClose was attached", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    core.emitClose();
    let closed = false;
    sock.onClose(() => {
      closed = true;
    });
    expect(closed).toBe(true);
  });

  it("never fires onError: there is no error event on this boundary", async () => {
    const sock = await localTransport().connectWebSocket("", { mode: "webpki" });
    let errored = false;
    sock.onError(() => {
      errored = true;
    });
    core.emitClose();
    expect(errored).toBe(false);
  });

  it("leaves no live subscription behind when the Core will not start", async () => {
    // Registering with the frame bus cannot fail, so a rejecting wsOpen means
    // the Core itself would not boot. The subscription made a moment earlier has
    // to come back off, or every retry adds another set of listeners to a Core
    // that is failing precisely because something is already wrong.
    core.failNextOpen();
    await expect(localTransport().connectWebSocket("", { mode: "webpki" })).rejects.toThrow(
      /would not start/,
    );
    expect(core.subscriberCount).toBe(0);
  });

  it("declares itself in process", () => {
    expect(localTransport().inProcess).toBe(true);
  });
});
