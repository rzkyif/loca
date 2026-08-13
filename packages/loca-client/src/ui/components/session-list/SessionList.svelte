<script lang="ts">
  // Session list mode: a scrollable list of clickable session bubbles, each
  // showing the session title and a conversation summary. Replaces the
  // prev/next session navigation. Which core's sessions are shown follows the
  // connected core; switching cores lives in the CoreBar pinned below. The live
  // session store drives a derived list of display rows fed to the shared
  // SessionListView, which owns all the markup.

  import { onMount } from "svelte";
  import type { SessionListEntry, SummaryPart } from "@loca/shared";
  import SessionListView, {
    type SessionRowView,
  } from "@loca/shared/ui/components/session-list/SessionListView.svelte";
  import { sessionsState, settingsState, viewState } from "$stores";
  import { formatSessionDefaultTitle } from "$lib/util/format";
  import { platform } from "$lib/platform";

  let confirmingDelete = $state<string | null>(null);

  onMount(() => {
    void sessionsState.loadList();
  });

  async function openSession(id: string): Promise<void> {
    await sessionsState.load(id);
    viewState.navigate("chat");
  }

  async function newSession(): Promise<void> {
    await sessionsState.create();
    viewState.navigate("chat");
  }

  async function deleteSession(id: string): Promise<void> {
    if (confirmingDelete !== id) {
      confirmingDelete = id;
      return;
    }
    confirmingDelete = null;
    await sessionsState.deleteById(id);
  }

  // Touch long-press opens an action sheet (the sheet IS the confirmation, so it
  // deletes directly rather than going through the two-tap icon-button confirm).
  // touch-only via the Bubble's longpress action, so desktop is unaffected.
  async function openSessionMenu(id: string): Promise<void> {
    const chosen = await platform().menu.showContextMenu([
      { id: "open", label: "Open" },
      { id: "delete", label: "Delete Session" },
    ]);
    if (chosen === "open") await openSession(id);
    else if (chosen === "delete") await sessionsState.deleteById(id);
  }

  // Untitled sessions fall back to their creation datetime, matching the
  // session bar's placeholder, so no session ever reads as "Untitled".
  function sessionTitle(entry: SessionListEntry): string {
    return entry.title.trim() || formatSessionDefaultTitle(entry.createdAtMs);
  }

  // Labels are rendered client-side so the configured agent name applies.
  let agentLabel = $derived(
    ((settingsState.currentSettings["general.context.agentName"] as string) ?? "").trim() ||
      "Agent",
  );

  function summaryText(parts: SummaryPart[]): string {
    return parts.map((p) => `${p.role === "user" ? "User" : agentLabel}: "${p.text}"`).join(" ");
  }

  let rows = $derived(
    sessionsState.list.map((entry): SessionRowView => ({
      id: entry.id,
      title: sessionTitle(entry),
      summary: entry.summary.length > 0 ? summaryText(entry.summary) : "",
      active: entry.id === sessionsState.id,
      confirmingDelete: confirmingDelete === entry.id,
    })),
  );
</script>

<SessionListView
  {rows}
  onSelect={(id) => void openSession(id)}
  onLongPress={(id) => void openSessionMenu(id)}
  onDelete={(id) => void deleteSession(id)}
  onNew={() => void newSession()}
  onBack={() => viewState.navigate("chat")}
/>
