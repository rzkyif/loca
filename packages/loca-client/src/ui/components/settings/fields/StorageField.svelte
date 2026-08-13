<script lang="ts">
  // Full on-disk storage browser. Driven by a provider chosen by `scope`: the
  // paired core (models, binaries, sessions, extensions, cache, logs, settings)
  // or the local client (settings, logs). The provider serves a tree with a
  // `lock_reason` on anything in use; this renders it, lets the user reclaim
  // disk, and routes every delete through the review modal. The provider
  // re-validates locks, so the client never decides what is safe to remove.
  import { onMount } from "svelte";
  import { errMessage, type SettingField } from "@loca/shared";
  import { cores } from "$lib/core";
  import { clientStorageProvider } from "$lib/core/client-storage";
  import { deletionsState, settingsState } from "../../../state";
  import { formatBytes } from "$lib/util/format";
  import {
    clearableNodes,
    expandToFiles,
    findNode,
    subtreeLockReason,
    visibleRows,
    type StorageCategory,
    type StorageNode,
    type StorageTree,
  } from "$lib/storage/tree";
  import { getLogger } from "$lib/util/log";
  import FieldCard from "./FieldCard.svelte";
  import StorageFieldView, {
    type StorageRowView,
    type StorageTreeView,
  } from "@loca/shared/ui/components/settings/StorageFieldView.svelte";

  const log = getLogger("storage");

  let { field, scope } = $props<{ field: SettingField; scope: "client" | "core" }>();

  let tree = $state<StorageTree | null>(null);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let expanded = $state<Set<string>>(new Set());
  let selected = $state<Set<string>>(new Set());
  let lastSelectedPath = $state<string | null>(null);

  // The storage provider for this scope. Both expose get/deletePaths/
  // clearCategory; the core one lives on the selected paired core.
  function provider() {
    return scope === "core" ? cores().api().storage : clientStorageProvider;
  }

  async function refresh() {
    loading = true;
    try {
      // Core scope with no paired/selected core: cores().api() would throw.
      if (scope === "core" && !cores().currentClient()) {
        tree = null;
        loadError = "No Core is connected.";
        return;
      }
      tree = await provider().get();
      loadError = null;
      // Drop selections for paths that no longer exist.
      const next = new Set<string>();
      for (const p of selected) if (tree && findNode(tree, p)) next.add(p);
      selected = next;
    } catch (e) {
      log.warn("load failed:", e);
      loadError = errMessage(e);
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  function toggle(key: string) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expanded = next;
  }

  function selectable(node: StorageNode, catId: string): boolean {
    // Settings is reset via its group's Clear button, not row selection.
    return !node.lock_reason && catId !== "settings";
  }

  function handleRowClick(e: MouseEvent, node: StorageNode, catId: string) {
    if (!selectable(node, catId)) {
      // Locked / settings rows aren't selectable, but folders still expand.
      if (node.kind === "folder" && !e.shiftKey && !e.metaKey && !e.ctrlKey) toggle(node.path);
      return;
    }
    const path = node.path;
    const next = new Set(selected);
    if (e.shiftKey && lastSelectedPath && tree) {
      const rows = visibleRows(tree, expanded)
        .filter((r) => selectable(r, catId))
        .map((r) => r.path);
      const a = rows.indexOf(lastSelectedPath);
      const b = rows.indexOf(path);
      if (a !== -1 && b !== -1) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        for (let i = lo; i <= hi; i++) next.add(rows[i]);
      } else {
        next.add(path);
      }
    } else if (e.metaKey || e.ctrlKey) {
      if (next.has(path)) next.delete(path);
      else next.add(path);
    } else {
      next.clear();
      next.add(path);
      if (node.kind === "folder") toggle(path);
    }
    selected = next;
    lastSelectedPath = path;
  }

  async function deletePaths(paths: string[]) {
    try {
      await provider().deletePaths(paths);
    } catch (err) {
      log.error("delete failed:", err);
    }
    await refresh();
  }

  // Open the review modal for the current multi-selection.
  function deleteSelected() {
    if (selected.size === 0 || !tree) return;
    const paths = expandToFiles(tree, [...selected]);
    const items: { label: string; sizeBytes: number }[] = [];
    const skipped: { label: string; reason: string }[] = [];
    const toDelete: string[] = [];
    for (const p of paths) {
      const n = findNode(tree, p);
      if (!n) continue;
      if (n.lock_reason) skipped.push({ label: n.name, reason: n.lock_reason });
      else {
        items.push({ label: n.name, sizeBytes: n.size });
        toDelete.push(p);
      }
    }
    if (items.length === 0 && skipped.length === 0) return;
    deletionsState.request({
      title: "Delete selected items",
      items,
      skipped,
      onConfirm: () => deletePaths(toDelete),
    });
  }

  // Open the review modal for a whole-category Clear.
  function clearCategory(cat: StorageCategory) {
    if (cat.id === "settings") {
      deletionsState.request({
        title: "Reset settings",
        notice:
          scope === "core"
            ? "This resets all Core settings to their defaults and removes saved API keys and passwords. This cannot be undone."
            : "This resets app settings to their defaults. Paired Cores and snippets are kept. This cannot be undone.",
        items: [],
        skipped: [],
        confirmLabel: "Reset",
        onConfirm: () => clearViaApi(cat.id),
      });
      return;
    }
    const items = clearableNodes(cat).map((n) => ({ label: n.name, sizeBytes: n.size }));
    const skipped = cat.nodes
      .map((n) => ({ n, reason: subtreeLockReason(n) }))
      .filter((x) => x.reason)
      .map((x) => ({ label: x.n.name, reason: x.reason! }));
    if (items.length === 0 && skipped.length === 0) return;
    deletionsState.request({
      title: `Clear ${cat.label.toLowerCase()}`,
      items,
      skipped,
      confirmLabel: "Clear",
      onConfirm: () => clearViaApi(cat.id),
    });
  }

  async function clearViaApi(categoryId: string) {
    try {
      await provider().clearCategory(categoryId);
      // A settings reset changes persisted settings; pull them back so the open
      // UI reflects the defaults instead of stale values.
      if (categoryId === "settings") await settingsState.loadSettings();
    } catch (err) {
      log.error("clear failed:", err);
    }
    await refresh();
  }

  function canClear(cat: StorageCategory): boolean {
    if (cat.id === "settings") return cat.nodes.length > 0;
    return clearableNodes(cat).length > 0;
  }

  function handleKeyDown(e: KeyboardEvent) {
    const isDelete = e.key === "Delete" || (e.key === "Backspace" && (e.metaKey || e.ctrlKey));
    if (isDelete && selected.size > 0) {
      e.preventDefault();
      deleteSelected();
    }
  }

  // Map a live tree node to the View's display shape: sizes pre-formatted via
  // formatBytes (a client helper), render flags precomputed, so the View is pure.
  function toRow(node: StorageNode, catId: string): StorageRowView {
    return {
      path: node.path,
      name: node.name,
      kind: node.kind,
      sizeText: formatBytes(node.size),
      locked: !!node.lock_reason,
      lockReason: node.lock_reason,
      selectable: selectable(node, catId),
      hasChildren: node.kind === "folder" && node.children.length > 0,
      children: node.kind === "folder" ? node.children.map((c) => toRow(c, catId)) : [],
    };
  }

  const viewTree = $derived<StorageTreeView | null>(
    tree
      ? {
          categories: tree.categories.map((cat) => ({
            id: cat.id,
            label: cat.label,
            sizeText: formatBytes(cat.size),
            canClear: canClear(cat),
            clearIcon:
              cat.id === "settings"
                ? "i-material-symbols-restart-alt-rounded"
                : "i-material-symbols-delete-outline-rounded",
            clearAriaLabel: cat.id === "settings" ? "Reset to defaults" : `Clear ${cat.label}`,
            clearTitle: cat.id === "settings" ? "Reset to defaults" : `Clear ${cat.label}`,
            settings: cat.id === "settings",
            nodes: cat.nodes.map((n) => toRow(n, cat.id)),
          })),
          totalSizeText: formatBytes(tree.total_size),
        }
      : null,
  );

  // The View hands back a path + category id; resolve the live node here so all
  // selection logic (range, lock checks, expand) stays in the client.
  function onRowClick(e: MouseEvent, path: string, catId: string) {
    const node = tree && findNode(tree, path);
    if (node) handleRowClick(e, node, catId);
  }

  function onClearCategory(catId: string) {
    const cat = tree?.categories.find((c) => c.id === catId);
    if (cat) clearCategory(cat);
  }
</script>

<FieldCard {field}>
  <StorageFieldView
    {loading}
    {loadError}
    tree={viewTree}
    {expanded}
    {selected}
    onToggle={toggle}
    {onRowClick}
    {onClearCategory}
    onKeyDown={handleKeyDown}
  />
</FieldCard>
