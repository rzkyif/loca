<script lang="ts">
  import { onMount } from "svelte";
  import {
    BUILTIN_EXTENSION_ID,
    errMessage,
    type Extension,
    type ExtensionSearchResult,
  } from "@loca/shared";
  import { confirmState, extensionsState } from "$stores";
  import type { InstallJob } from "$stores/extensions.svelte";
  import { cores } from "$lib/core";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu, showObjectActionMenu } from "$lib/objects/menu";
  import type { Badge } from "@loca/shared/ui/components/objects/object-types.ts";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import ExtensionsFieldView from "@loca/shared/ui/components/settings/ExtensionsFieldView.svelte";
  import ExtensionDetail from "./ExtensionDetail.svelte";

  // A list row is either an installed extension or an npm search result. A single
  // load returns one kind (installed XOR npm), so ids never collide.
  type ExtensionItem =
    | { kind: "installed"; extension: Extension }
    | { kind: "npm"; result: ExtensionSearchResult; installed: boolean };

  let { horizontal = false }: { horizontal?: boolean } = $props();

  let query = $state("");
  let selectedItem = $state<ExtensionItem | null>(null);
  let reloadKey = $state(0);
  // True while "Check for Updates" runs; swaps the triple-dot for a spinner.
  let checking = $state(false);
  // Non-job detail action in flight (currently just "uninstall"); drives that
  // button's loading state. Download/install/update loading is derived from the
  // streamed job instead (extensionsState.isJobRunning).
  let busyAction = $state<string | null>(null);

  onMount(() => void extensionsState.attach());

  function isUpdatable(tk: Extension): boolean {
    return extensionsState.updateStatus[tk.id]?.updateAvailable ?? false;
  }

  // The extension lifecycle as a single badge: drift / update / downloaded /
  // installed. Failures are NOT a extension state (they surface transiently via
  // confirmState.alert), so there is no "Error" badge.
  function statusBadge(tk: Extension): Badge {
    if (tk.status === "drift") {
      return {
        label: "Content changed",
        accent: "red",
        title: "Files changed on disk since install; review and re-enable.",
      };
    }
    if (isUpdatable(tk)) {
      const latest = extensionsState.updateStatus[tk.id]?.latestVersion;
      return {
        label: "Update available",
        accent: "yellow",
        title: latest ? `v${latest}` : undefined,
      };
    }
    if (tk.status === "downloaded") return { label: "Install to enable", accent: "yellow" };
    return { label: "Installed", accent: "green" };
  }

  // An installed extension also shows how many of its tools are enabled.
  function extensionBadges(tk: Extension): Badge[] {
    const badges = [statusBadge(tk)];
    if (tk.status === "installed") badges.push({ label: `${tk.enabledToolCount} enabled` });
    return badges;
  }

  // Tail of an install job's stderr, used as the failure message so the real
  // deno error is shown instead of a bare exit code.
  function stderrTail(job: InstallJob): string {
    return job.lines
      .filter((l) => l.line.trim())
      .slice(-12)
      .map((l) => l.line)
      .join("\n");
  }

  // Start a streamed job (download/install/update), wait for it to finish, and
  // surface a failure transiently with the real stderr. Sync REST errors (e.g. a
  // 404/409) are caught the same way.
  async function runJob(title: string, start: () => Promise<string>): Promise<void> {
    try {
      const jobId = await start();
      const job = await extensionsState.awaitJob(jobId);
      if (job.status === "failed") {
        confirmState.alert({ title, message: stderrTail(job) || "Failed. See logs for details." });
      }
    } catch (e) {
      confirmState.alert({ title, message: errMessage(e) });
    }
  }

  function updateRow(tk: Extension): MenuRow {
    const latest = extensionsState.updateStatus[tk.id]?.latestVersion;
    return {
      id: "update",
      label: latest ? `Update to v${latest}` : "Update",
      onSelect: () => runJob("Update failed", () => extensionsState.updateExtension(tk.id)),
    };
  }

  // Extension ids flatten an npm `@scope/name` to `@scope__name`.
  function isInstalled(name: string): boolean {
    return extensionsState.installed.some((t) => t.id === name || t.id === name.replace("/", "__"));
  }

  function isInstalling(name: string): boolean {
    return Object.values(extensionsState.installJobs).some(
      (j) => j.status === "running" && j.label === name,
    );
  }

  // Phase 2 action for a downloaded extension: install its deps so its tools can
  // be enabled. (Per-tool enable lives in the detail view.)
  function installRow(tk: Extension): MenuRow {
    return {
      id: "install",
      label: "Install",
      onSelect: () => runJob("Install failed", () => extensionsState.installDeps(tk.id)),
    };
  }

  function reenableRow(tk: Extension): MenuRow {
    return { id: "reenable", label: "Review & re-enable", onSelect: () => confirmReenable(tk) };
  }

  // Drift confirm: the user reviewed the on-disk change and trusts it. Re-pins
  // the hash and returns the extension to installed; its tools stay disabled.
  function confirmReenable(tk: Extension, after?: () => void) {
    confirmState.request({
      title: "Content changed",
      message:
        `"${tk.displayName || tk.id}" files changed on disk since it was installed. ` +
        `Re-enabling trusts the current on-disk contents and re-pins its hash. Only continue ` +
        `if you made this change. Its tools stay disabled until you re-enable them.`,
      destructive: true,
      confirmLabel: "Trust and re-enable",
      onConfirm: async () => {
        try {
          await extensionsState.confirmReenable(tk.id);
          after?.();
        } catch (e) {
          confirmState.alert({ title: "Re-enable failed", message: errMessage(e) });
        }
      },
    });
  }

  // Uninstall reverts an installed, deps-bearing extension to 'downloaded'. A
  // no-dep extension (installed on download) has nothing to uninstall, so it is
  // delete-only; an already-downloaded/drifted extension is delete-only too.
  function canUninstall(tk: Extension): boolean {
    return tk.status === "installed" && tk.hasDeps;
  }

  // Uninstall: drop installed deps, revert to 'downloaded'. The extension stays
  // (re-installable), so the detail view is left open.
  function uninstallExtension(tk: Extension) {
    confirmState.request({
      title: "Uninstall extension",
      message:
        `Uninstall "${tk.displayName || tk.id}"? Its installed dependencies are removed and ` +
        `its tools are disabled until you Install again. The extension stays so you can re-install.`,
      destructive: true,
      confirmLabel: "Uninstall",
      onConfirm: async () => {
        busyAction = "remove";
        try {
          await extensionsState.uninstall(tk.id);
        } catch (e) {
          confirmState.alert({ title: "Uninstall failed", message: errMessage(e) });
        } finally {
          busyAction = null;
        }
      },
    });
  }

  // Delete: remove the extension's grants + files entirely. Exits the detail view
  // and reloads the list so the deleted extension disappears.
  function deleteExtension(tk: Extension, after?: () => void) {
    confirmState.request({
      title: "Delete extension",
      message: `Delete "${tk.displayName || tk.id}" v${tk.version}? Its grants and installed files will be removed.`,
      destructive: true,
      confirmLabel: "Delete",
      onConfirm: async () => {
        busyAction = "remove";
        try {
          await extensionsState.deleteExtension(tk.id);
          after?.();
          reloadKey++;
        } catch (e) {
          confirmState.alert({ title: "Delete failed", message: errMessage(e) });
        } finally {
          busyAction = null;
        }
      },
    });
  }

  function cardMenuRows(item: ExtensionItem): MenuRow[] {
    if (item.kind === "installed") {
      const tk = item.extension;
      const rows: MenuRow[] = [];
      if (tk.status === "downloaded") rows.push(installRow(tk));
      if (tk.status === "drift") rows.push(reenableRow(tk));
      if (isUpdatable(tk)) rows.push(updateRow(tk));
      rows.push(
        canUninstall(tk)
          ? { id: "uninstall", label: "Uninstall", onSelect: () => uninstallExtension(tk) }
          : { id: "delete", label: "Delete", onSelect: () => deleteExtension(tk) },
      );
      return rows;
    }
    if (item.installed || isInstalling(item.result.name)) return [];
    return [
      {
        id: "download",
        label: "Download",
        onSelect: () =>
          runJob("Download failed", () =>
            extensionsState.download({ source: "npm", name: item.result.name }),
          ),
      },
    ];
  }

  function npmMeta(result: ExtensionSearchResult): string {
    return [
      `v${result.version}`,
      result.weeklyDownloads !== undefined
        ? `${result.weeklyDownloads.toLocaleString()} weekly downloads`
        : null,
    ]
      .filter((x): x is string => x !== null)
      .join(" · ");
  }

  function npmBadges(result: ExtensionSearchResult, installed: boolean): Badge[] {
    if (installed) return [{ label: "Installed", accent: "green" }];
    if (isInstalling(result.name)) return [{ label: "Installing", accent: "yellow" }];
    return [{ label: "Downloadable" }];
  }

  async function load({
    query: q,
    offset,
    limit,
  }: {
    offset: number;
    limit: number;
    query: ParsedQuery;
  }): Promise<{ items: ExtensionItem[]; done: boolean }> {
    // Pull a fresh installed list first: it backs both the installed view and
    // the npm view's "Installed" flag, decoupled from the WS refresh timing.
    await extensionsState.refresh();

    if (q.filters.has("npm")) {
      const text = q.text.trim();
      if (!text) return { items: [], done: true };
      const res = await cores().api().extensions.search(text, { offset, limit });
      const items: ExtensionItem[] = res.results.map((result) => ({
        kind: "npm",
        result,
        installed: isInstalled(result.name),
      }));
      return { items, done: res.results.length < limit };
    }

    const text = q.text.toLowerCase();
    let list = extensionsState.installed.filter(
      (t) =>
        !text ||
        (t.displayName || t.id).toLowerCase().includes(text) ||
        (t.description ?? "").toLowerCase().includes(text),
    );
    // `@update-available` narrows to extensions a prior check flagged.
    if (q.filters.has("update-available")) {
      list = list.filter((t) => isUpdatable(t));
    }
    // `downloads` is meaningful only for npm; installed falls back to name.
    if (q.sort === "name" || q.sort === "downloads") {
      list = [...list].sort((a, b) => (a.displayName || a.id).localeCompare(b.displayName || b.id));
    }
    return { items: list.map((extension) => ({ kind: "installed", extension })), done: true };
  }

  // Which empty-state the field is in, derived from the live query. npm searches
  // prompt vs report no results; the installed list reports no matches vs none.
  function emptyMode(): "npmEmpty" | "npmNoResults" | "noMatches" | "none" {
    if (query.includes("@npm")) {
      return query.replace(/@npm/g, "").trim() ? "npmNoResults" : "npmEmpty";
    }
    return query.trim() ? "noMatches" : "none";
  }

  function detailActions(tk: Extension, close: () => void) {
    // Download/install/update run as streamed jobs; their button shows a spinner
    // for the whole job. Uninstall is a direct call tracked by busyAction. While
    // any action is in flight the others are disabled.
    const jobRunning = extensionsState.isJobRunning(tk.id);
    const anyBusy = jobRunning || busyAction !== null;
    const actions = [];
    if (tk.status === "downloaded") {
      actions.push({
        label: "Install",
        icon: "i-material-symbols-download-rounded",
        loading: jobRunning,
        disabled: anyBusy && !jobRunning,
        onSelect: () => installRow(tk).onSelect(),
      });
    }
    if (tk.status === "drift") {
      actions.push({
        label: "Review & re-enable",
        icon: "i-material-symbols-warning-outline-rounded",
        disabled: anyBusy,
        onSelect: () => confirmReenable(tk, close),
      });
    }
    if (isUpdatable(tk)) {
      const latest = extensionsState.updateStatus[tk.id]?.latestVersion;
      actions.push({
        label: latest ? `Update to v${latest}` : "Update",
        icon: "i-material-symbols-upgrade-rounded",
        loading: jobRunning,
        disabled: anyBusy && !jobRunning,
        onSelect: () => updateRow(tk).onSelect(),
      });
    }
    // Removal: an installed, deps-bearing extension is Uninstalled (revert); every
    // other state is delete-only.
    if (canUninstall(tk)) {
      actions.push({
        label: "Uninstall",
        icon: "i-material-symbols-package-2-outline",
        loading: busyAction === "remove",
        disabled: anyBusy && busyAction !== "remove",
        onSelect: () => uninstallExtension(tk),
      });
    } else {
      actions.push({
        label: "Delete",
        icon: "i-material-symbols-delete-outline-rounded",
        variant: "danger" as const,
        loading: busyAction === "remove",
        disabled: anyBusy && busyAction !== "remove",
        onSelect: () => deleteExtension(tk, close),
      });
    }
    return actions;
  }
</script>

<ObjectManager
  {load}
  idOf={(item) => (item.kind === "installed" ? item.extension.id : item.result.name)}
  getById={(id): ExtensionItem | undefined => {
    const extension = extensionsState.installed.find((t) => t.id === id);
    return extension ? { kind: "installed", extension } : undefined;
  }}
  searchPlaceholder="Search extensions"
  subscribe={(onChange) =>
    cores().subscribeWs((f) => {
      if (f.kind === "extension.snapshot" || f.kind === "extension.install_done") onChange();
    })}
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [
        {
          label: "Source",
          options: [
            { token: "installed", label: "Installed" },
            { token: "npm", label: "npm Marketplace" },
          ],
        },
        { label: "Status", options: [{ token: "update-available", label: "Update available" }] },
      ],
      sorts: [
        { value: "name", label: "Name" },
        { value: "downloads", label: "Downloads" },
      ],
      query,
      onQueryChange: (q) => (query = q),
    })}
  hasMenu
  menuBusy={checking}
  onMenu={() => {
    const rows: MenuRow[] = [
      {
        id: "check-updates",
        label: "Check for Updates",
        onSelect: async () => {
          checking = true;
          try {
            await extensionsState.checkUpdates();
            // Narrow to the installed extensions that have an update available.
            query = "@installed @update-available";
            reloadKey++;
          } catch (e) {
            confirmState.alert({ title: "Check for updates failed", message: errMessage(e) });
          } finally {
            checking = false;
          }
        },
      },
    ];
    if (!extensionsState.isBuiltinInstalled && !isInstalling(BUILTIN_EXTENSION_ID)) {
      rows.push({
        id: "download-builtin",
        label: "Download built-in extension",
        onSelect: () => runJob("Download failed", () => extensionsState.downloadBuiltin()),
      });
    }
    rows.push({
      id: "rescan",
      label: "Rescan Local Extensions",
      onSelect: async () => {
        try {
          await extensionsState.rescan();
          // Reindexing is part of a rescan, not its own action: new/changed
          // tools must be re-embedded for relevance. If it fails, so does the
          // rescan.
          await extensionsState.reindex();
          reloadKey++;
        } catch (e) {
          confirmState.alert({ title: "Rescan failed", message: errMessage(e) });
        }
      },
    });
    showObjectActionMenu(rows);
  }}
>
  {#snippet card(item, open)}
    {#if item.kind === "installed"}
      <ObjectCard
        label={item.extension.displayName || item.extension.id}
        description={item.extension.description}
        meta={`v${item.extension.version}`}
        badges={extensionBadges(item.extension)}
        menuRows={cardMenuRows(item)}
        onOpen={open}
      />
    {:else}
      <ObjectCard
        label={item.result.name}
        description={item.result.description}
        meta={npmMeta(item.result)}
        badges={npmBadges(item.result, item.installed)}
        menuRows={cardMenuRows(item)}
      />
    {/if}
  {/snippet}
  {#snippet detail(item, close)}
    {#if item.kind === "installed"}
      {@const tk = item.extension}
      <ObjectDetailHeader
        title={tk.displayName || tk.id}
        subtitle={`v${tk.version} · ${tk.source}`}
        badges={extensionBadges(tk)}
        actions={detailActions(tk, close)}
      />
      <ObjectDetailScroll description={tk.description}>
        <ExtensionDetail extension={tk} {horizontal} />
      </ObjectDetailScroll>
    {/if}
  {/snippet}
  {#snippet empty()}
    <ExtensionsFieldView mode={emptyMode()} />
  {/snippet}
</ObjectManager>
