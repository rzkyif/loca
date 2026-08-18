// The Cores detail, opened on the Core inside this app.
//
// Two things must not happen there, and both are the same mistake one step
// apart. The detail's health probe is a real outbound `platform().net.fetch`,
// and the reserved entry's base URL is a sentinel that exists to make such a
// request fail loudly, so probing it is an escape attempt on behalf of a Core
// that is already in this process. And an embedded Core has no listener, so it
// accepts no inbound pairing at all: offering "Generate Pairing Code" for it is
// a button whose only outcome is a password prompt against a Core that would
// refuse the result.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/svelte";

const h = vi.hoisted(() => ({
  probeCore: vi.fn(),
  listClients: vi.fn(),
  entries: [] as Array<{
    id: string;
    name: string;
    baseUrl: string;
    trustMode: string;
    addedAtMs: number;
  }>,
}));

vi.mock("$lib/core", () => ({
  cores: () => ({
    list: () => Promise.resolve(h.entries),
    currentEntry: () => h.entries[0] ?? null,
    subscribe: () => () => {},
    api: () => ({ pairing: { listClients: h.listClients } }),
    rename: vi.fn(),
    select: vi.fn(),
    removePaired: vi.fn(),
  }),
  isLocalCoreEntry: (e: { id: string }) => e.id === "local",
  isThisDevicesCore: () => false,
  mintCodeWithAdminToken: vi.fn(),
  probeCore: h.probeCore,
  setAdminPasswordWithToken: vi.fn(),
}));
vi.mock("$lib/platform", () => ({
  platform: () => ({
    pairing: {
      localCoreBaseUrl: () => Promise.resolve(""),
      readAdminToken: () => Promise.resolve(null),
    },
  }),
}));
vi.mock("$stores", () => ({
  confirmState: { request: vi.fn(), alert: vi.fn() },
  passwordPromptState: { request: vi.fn() },
  viewState: { navigate: vi.fn(), setLocked: vi.fn(), pendingSettingsGroup: null },
}));
vi.mock("$lib/objects/menu", () => ({
  showFilterSortMenu: vi.fn(),
  showObjectActionMenu: vi.fn(),
}));

import CoresField from "./CoresField.svelte";

const LOCAL = {
  id: "local",
  name: "This Device",
  baseUrl: "https://local.loca.invalid",
  trustMode: "webpki",
  addedAtMs: 1,
};
const REMOTE = {
  id: "core-a",
  name: "Studio Mac",
  baseUrl: "https://192.168.1.20:8787",
  trustMode: "pin",
  addedAtMs: 1,
};

async function openDetail(name: string) {
  const screen = render(CoresField);
  await waitFor(() => expect(screen.getByLabelText(`Open ${name}`)).toBeTruthy());
  await fireEvent.click(screen.getByLabelText(`Open ${name}`));
  await waitFor(() => expect(screen.getByLabelText("Core name")).toBeTruthy());
  return screen;
}

describe("CoresField detail", () => {
  it("neither probes nor offers a pairing code for the Core inside this app", async () => {
    h.probeCore.mockResolvedValue({ version: "1", behindProxy: false, adminPasswordSet: false });
    h.listClients.mockResolvedValue([]);
    h.entries = [LOCAL];

    const { queryByText } = await openDetail("This Device");

    expect(h.probeCore).not.toHaveBeenCalled();
    expect(queryByText("Generate Pairing Code")).toBeNull();
  });

  it("still probes and still offers a code for an ordinary Core", async () => {
    // The other half of the gate: without this, hiding the affordance for
    // everything would satisfy the assertion above.
    h.probeCore.mockResolvedValue({ version: "1", behindProxy: false, adminPasswordSet: true });
    h.listClients.mockResolvedValue([]);
    h.entries = [REMOTE];

    const { getByText } = await openDetail("Studio Mac");

    expect(h.probeCore).toHaveBeenCalledWith(REMOTE.baseUrl);
    expect(getByText("Generate Pairing Code")).toBeTruthy();
  });
});
