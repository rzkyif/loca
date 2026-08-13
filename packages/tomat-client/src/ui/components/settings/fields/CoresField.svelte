<script lang="ts">
  import { errMessage, MIN_ADMIN_PASSWORD_LENGTH, type PairedClientEntry } from "@tomat/shared";
  import {
    cores,
    isThisDevicesCore,
    mintCodeWithAdminToken,
    type PairedCoreEntry,
    probeCore,
    setAdminPasswordWithToken,
  } from "$lib/core";
  import { platform } from "$lib/platform";
  import { confirmState, passwordPromptState, viewState } from "$stores";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu, showObjectActionMenu } from "$lib/objects/menu";
  import { createDebouncedSave } from "$lib/util/debounced-save";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@tomat/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@tomat/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import CoresFieldView from "@tomat/shared/ui/components/settings/CoresFieldView.svelte";
  import type { Badge } from "@tomat/shared/ui/components/objects/object-types.ts";

  let query = $state("");
  let selectedItem = $state<PairedCoreEntry | null>(null);
  let reloadKey = $state(0);

  // Cores have no sync getById, so the detail works off the snapshot. The rename
  // draft is field-level (so the header can reflect it live) and re-inits when a
  // different core is opened. Reads selectedItem only, so typing won't reset it.
  let draftName = $state("");
  let savedName = $state("");

  // Pairing-key generation + the paired-devices list are only available for the
  // CURRENT core, because we hold a live bearer connection (CoreClient) only for
  // it. Both reset when a different core is opened.
  let devices = $state<PairedClientEntry[] | null>(null);
  let devicesError = $state<string | null>(null);
  let mintedCode = $state<string | null>(null);
  let mintedExpiresAtMs = $state<number | null>(null);
  let codeCopied = $state(false);
  // Whether the opened core has an admin password (from its health probe; null
  // while unknown). A same-device core without one can still mint codes and set
  // the password via its on-disk admin token, so the detail adapts to it.
  let adminPasswordSet = $state<boolean | null>(null);

  $effect(() => {
    const sel = selectedItem;
    draftName = sel?.name ?? "";
    savedName = sel?.name ?? "";
    // Clear per-core transient UI and (re)load the device list for the current.
    devices = null;
    devicesError = null;
    mintedCode = null;
    mintedExpiresAtMs = null;
    codeCopied = false;
    adminPasswordSet = null;
    if (sel && isCurrent(sel.id)) {
      void loadDevices();
      void probeCore(sel.baseUrl)
        .then((h) => (adminPasswordSet = h.adminPasswordSet))
        .catch(() => (adminPasswordSet = null));
    }
  });

  function isCurrent(id: string): boolean {
    return cores().currentEntry()?.id === id;
  }

  // The on-disk admin token only authorizes the Core the installer put on THIS
  // device, so token-gated actions (password management, promptless minting)
  // match the platform-resolved local base URL exactly (isThisDevicesCore).
  let localBaseUrl = $state<string | null>(null);
  void platform()
    .pairing.localCoreBaseUrl()
    .then((u) => (localBaseUrl = u))
    .catch(() => (localBaseUrl = null));

  function isLocal(c: PairedCoreEntry): boolean {
    return isThisDevicesCore(c.baseUrl, localBaseUrl);
  }

  // Card / detail badges: the "Current" marker plus a read-only trust indicator
  // so the user can see whether Tomat secures this Core itself (Pinned) or trusts
  // the HTTPS certificate of the reverse proxy in front of it.
  function coreBadges(item: PairedCoreEntry): Badge[] {
    const badges: Badge[] = [];
    if (isCurrent(item.id)) badges.push({ label: "Current", accent: "green" });
    badges.push(
      item.trustMode === "webpki"
        ? {
            label: "HTTPS",
            accent: "blue",
            title: "Served over HTTPS by a reverse proxy; Tomat trusts its certificate.",
          }
        : {
            label: "Pinned",
            title: "Tomat secures this connection with a pinned certificate.",
          },
    );
    return badges;
  }

  async function loadDevices() {
    try {
      devices = await cores().api().pairing.listClients();
      devicesError = null;
    } catch (e) {
      devicesError = errMessage(e);
    }
  }

  // Generate a pairing code for another device. A local core without an admin
  // password mints straight off its on-disk admin token (no prompt); otherwise
  // the password modal stays open on a wrong password (onSubmit throws) and on
  // success closes, showing the minted code inline for the new device. The
  // token path also covers an unsettled/failed probe (adminPasswordSet null):
  // the core accepts the admin token regardless of whether a password exists,
  // while the password prompt would dead-end on a passwordless core.
  function generatePairingCode(c: PairedCoreEntry) {
    if (isLocal(c) && adminPasswordSet !== true) {
      void (async () => {
        try {
          const adminToken = await platform().pairing.readAdminToken();
          if (!adminToken) throw new Error("local Core admin token not found");
          const res = await mintCodeWithAdminToken(c.baseUrl, adminToken);
          mintedCode = res.code;
          mintedExpiresAtMs = res.expiresAtMs;
          codeCopied = false;
        } catch (e) {
          confirmState.alert({ title: "Could not generate a code", message: errMessage(e) });
        }
      })();
      return;
    }
    passwordPromptState.request({
      title: "Generate a pairing code",
      message: "Enter your admin password to create a code for pairing a new device.",
      confirmLabel: "Generate",
      onSubmit: async (password) => {
        const res = await cores().api().pairing.mintCodeWithPassword(password);
        mintedCode = res.code;
        mintedExpiresAtMs = res.expiresAtMs;
        codeCopied = false;
      },
    });
  }

  // Set or replace the local core's admin password, authorized by the on-disk
  // admin token (so no old password is needed; owning the device is the proof).
  // The installer never asks for a password, so this is where one is created.
  function setAdminPassword(c: PairedCoreEntry) {
    passwordPromptState.request({
      title: adminPasswordSet ? "Replace admin password" : "Set admin password",
      message:
        `Choose a password of at least ${MIN_ADMIN_PASSWORD_LENGTH} characters. ` +
        "You'll use it to pair new devices and remove old ones from any paired device.",
      confirmLabel: adminPasswordSet ? "Replace" : "Set",
      onSubmit: async (password) => {
        if (password.length < MIN_ADMIN_PASSWORD_LENGTH) {
          throw new Error(`Use at least ${MIN_ADMIN_PASSWORD_LENGTH} characters.`);
        }
        const adminToken = await platform().pairing.readAdminToken();
        if (!adminToken) throw new Error("local Core admin token not found");
        await setAdminPasswordWithToken(c.baseUrl, adminToken, password);
        adminPasswordSet = true;
      },
    });
  }

  async function copyCode() {
    if (!mintedCode) return;
    try {
      await navigator.clipboard.writeText(mintedCode);
      codeCopied = true;
      setTimeout(() => (codeCopied = false), 1500);
    } catch {
      /* clipboard unavailable; the code is still shown to read */
    }
  }

  // Remove another paired device. Privileged, so it asks for the admin password.
  function removeDevice(d: PairedClientEntry) {
    passwordPromptState.request({
      title: "Remove device",
      message: `Enter your admin password to remove "${d.name}". It will need a new pairing key to reconnect.`,
      confirmLabel: "Remove",
      onSubmit: async (password) => {
        await cores().api().pairing.revoke(d.id, password);
        await loadDevices();
      },
    });
  }

  // Compact "expires in N minutes" / "last seen" strings. Minute granularity is
  // plenty for a pairing code's lifetime and a device's last-seen time.
  function expiresInLabel(expiresAtMs: number): string {
    const mins = Math.max(0, Math.round((expiresAtMs - Date.now()) / 60000));
    if (mins <= 0) return "expired";
    return `expires in ${mins} minute${mins === 1 ? "" : "s"}`;
  }

  function lastSeenLabel(lastSeenMs: number): string {
    const mins = Math.round((Date.now() - lastSeenMs) / 60000);
    if (mins < 1) return "active now";
    if (mins < 60) return `last seen ${mins} minute${mins === 1 ? "" : "s"} ago`;
    const hours = Math.round(mins / 60);
    if (hours < 24) return `last seen ${hours} hour${hours === 1 ? "" : "s"} ago`;
    const days = Math.round(hours / 24);
    return `last seen ${days} day${days === 1 ? "" : "s"} ago`;
  }

  const { scheduleSave, flushSave } = createDebouncedSave(async () => {
    const id = selectedItem?.id;
    const name = draftName.trim();
    if (!id || !name || name === savedName) return;
    try {
      await cores().rename(id, name);
      savedName = name;
    } catch (e) {
      confirmState.alert({ title: "Rename failed", message: errMessage(e) });
    }
  });

  async function doSwitch(c: PairedCoreEntry) {
    try {
      await cores().select(c.id);
    } catch (e) {
      confirmState.alert({ title: "Switch failed", message: errMessage(e) });
    }
  }

  function doUnpair(c: PairedCoreEntry, after?: () => void) {
    confirmState.request({
      title: "Unpair Core",
      message: `Unpair "${c.name}"? You will need to pair again to reconnect.`,
      destructive: true,
      confirmLabel: "Unpair",
      onConfirm: async () => {
        await cores().removePaired(c.id);
        // Lock back to the add-core flow if none remain (setLocked auto-navigates
        // to newCore); otherwise make sure some core is active again when the
        // removed one was the current one.
        const remaining = await cores().list();
        if (remaining.length === 0) viewState.setLocked(true);
        else if (!cores().currentEntry()) await cores().select(remaining[0].id);
        after?.();
      },
    });
  }

  function cardMenuRows(c: PairedCoreEntry): MenuRow[] {
    const rows: MenuRow[] = [];
    if (!isCurrent(c.id)) {
      rows.push({ id: "switch", label: "Switch to This Core", onSelect: () => void doSwitch(c) });
    }
    rows.push({ id: "unpair", label: "Unpair", onSelect: () => doUnpair(c) });
    return rows;
  }

  function detailActions(c: PairedCoreEntry, close: () => void) {
    const actions = [];
    if (!isCurrent(c.id)) {
      actions.push({
        label: "Switch",
        icon: "i-material-symbols-swap-horiz-rounded",
        onSelect: () => void doSwitch(c),
      });
    }
    actions.push({
      label: "Unpair",
      icon: "i-material-symbols-link-off-rounded",
      variant: "danger" as const,
      onSelect: () => doUnpair(c, close),
    });
    return actions;
  }

  async function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    const all = await cores().list();
    const text = q.text.toLowerCase();
    let items = all.filter(
      (c) => !text || c.name.toLowerCase().includes(text) || c.baseUrl.toLowerCase().includes(text),
    );
    if (q.sort === "name") items = [...items].sort((a, b) => a.name.localeCompare(b.name));
    return { items, done: true };
  }
</script>

<ObjectManager
  {load}
  idOf={(c) => c.id}
  searchPlaceholder="Search paired Cores"
  subscribe={(onChange) => cores().subscribe(onChange)}
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [],
      sorts: [{ value: "name", label: "Name" }],
      query,
      onQueryChange: (q) => (query = q),
    })}
  hasMenu
  onMenu={() =>
    showObjectActionMenu([
      { id: "pair", label: "Pair New Core", onSelect: () => viewState.navigate("newCore") },
    ])}
>
  {#snippet card(item, open)}
    <ObjectCard
      label={item.name}
      description={item.baseUrl}
      badges={coreBadges(item)}
      menuRows={cardMenuRows(item)}
      onOpen={open}
    />
  {/snippet}
  {#snippet detail(item, close)}
    <ObjectDetailHeader
      title={draftName.trim() || item.name}
      badges={coreBadges(item)}
      actions={detailActions(item, close)}
    />
    <ObjectDetailScroll>
      <CoresFieldView
        {draftName}
        baseUrl={item.baseUrl}
        isCurrent={isCurrent(item.id)}
        {mintedCode}
        mintedExpiresLabel={mintedExpiresAtMs ? expiresInLabel(mintedExpiresAtMs) : null}
        {codeCopied}
        devices={devices?.map((d) => ({
          id: d.id,
          name: d.name,
          lastSeenLabel: lastSeenLabel(d.lastSeenMs),
          isMe: d.isMe,
        })) ?? null}
        {devicesError}
        {adminPasswordSet}
        canSetPassword={isLocal(item)}
        onNameInput={(v) => {
          draftName = v;
          scheduleSave();
        }}
        onNameBlur={() => flushSave()}
        onGenerateCode={() => generatePairingCode(item)}
        onCopyCode={() => copyCode()}
        onRemoveDevice={(id) => {
          const d = devices?.find((x) => x.id === id);
          if (d) removeDevice(d);
        }}
        onSetPassword={() => setAdminPassword(item)}
      />
    </ObjectDetailScroll>
  {/snippet}
  {#snippet empty()}
    <div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
      {#if query.trim()}
        <div class="text-base text-default-700">No matching Cores</div>
      {:else}
        <div class="text-base text-default-700">No paired Cores</div>
        <div class="text-sm text-default-500">Use the menu to pair a new Core.</div>
      {/if}
    </div>
  {/snippet}
</ObjectManager>
