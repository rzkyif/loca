<script lang="ts">
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import MessageEnter from "./MessageEnter.svelte";
  import { useUiContext } from "@loca/shared/ui/context";
  import { type BubbleMerge, NO_MERGE } from "@loca/shared/ui/merge";
  import { messagesState, sessionsState, settingsState, viewState } from "../../state";
  import { connectionState } from "$stores/connection.svelte";

  // Mobile rides the panel carousel for screen entry, so the per-bar entry slide
  // is gated off there (it would double up with the carousel); desktop keeps it.
  const onMobile = useUiContext().platform === "mobile";
  import { hasAlpha } from "$lib/appearance/color";

  // The stacking z-index for this row, supplied by the chat column so a row
  // lower on screen paints over the ones above it. Owned here (not in a +page
  // wrapper) so a hidden bar leaves no empty flex item behind.
  let {
    zIndex,
    merge = NO_MERGE,
    onWidth = undefined,
  }: {
    zIndex: number;
    merge?: BubbleMerge;
    onWidth?: (width: number) => void;
  } = $props();

  // Until a /api/v1/llm/status endpoint exists, fall back to llama.cpp's
  // 8192 default.
  function getContextSize(): number {
    return Number(settingsState.currentSettings["llm.contextSize"]) || 8192;
  }

  const themeOverride = $derived(
    settingsState.currentSettings["appearance.sessionBarDefaultColor"] as string,
  );
  const themeOverrideHex = $derived(hasAlpha(themeOverride) ? themeOverride : null);

  let titleInput: HTMLInputElement | undefined = $state();
  let titleText = $state(sessionsState.title);
  let editingTitle = $state(false);
  let confirmingDelete = $state(false);

  // Sync title from state
  $effect(() => {
    if (!editingTitle) {
      titleText = sessionsState.title;
    }
  });

  function handleTitleFocus() {
    editingTitle = true;
    confirmingDelete = false;
  }

  function handleTitleBlur() {
    editingTitle = false;
    const trimmed = titleText.trim();
    if (!trimmed) {
      titleText = sessionsState.defaultTitle;
      sessionsState.updateTitle(titleText);
    } else if (trimmed !== sessionsState.title) {
      sessionsState.updateTitle(trimmed);
    }
  }

  function handleTitleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      titleInput?.blur();
    }
  }

  // Cancel confirm-delete when any other interactive element is clicked/focused
  function handleWindowFocusIn(e: FocusEvent) {
    if (!confirmingDelete) return;
    const target = e.target as HTMLElement;
    if (!target.closest("[data-delete-btn]")) {
      confirmingDelete = false;
    }
  }

  $effect(() => {
    if (confirmingDelete) {
      window.addEventListener("focusin", handleWindowFocusIn, true);
      return () => {
        window.removeEventListener("focusin", handleWindowFocusIn, true);
      };
    }
  });

  function handleDeleteClick() {
    if (confirmingDelete) {
      confirmingDelete = false;
      sessionsState.delete();
    } else {
      confirmingDelete = true;
    }
  }

  // Context progress (the gauge itself lives in SessionBarView).
  let contextMax = $derived(getContextSize());
  let contextUsed = $derived(messagesState.tokenUsage?.total || 0);

  // Prev/next session targets. The list is newest-first, so "previous"
  // walks toward older sessions (higher index) and "next" toward newer
  // ones. currentIndex is -1 for a brand-new session not yet in the list;
  // treat that as sitting before the newest entry, so "previous" lands on
  // the newest stored session and "next" has nowhere to go.
  let prevSession = $derived(
    sessionsState.currentIndex === -1
      ? (sessionsState.list[0] ?? null)
      : (sessionsState.list[sessionsState.currentIndex + 1] ?? null),
  );
  let nextSession = $derived(
    sessionsState.currentIndex > 0
      ? (sessionsState.list[sessionsState.currentIndex - 1] ?? null)
      : null,
  );

  function goToSession(id: string) {
    confirmingDelete = false;
    void sessionsState.load(id);
  }

  let defaultTitle = $derived(sessionsState.defaultTitle);
  let isNewSession = $derived(messagesState.messages.length === 0);

  // The editable title is for persistent sessions; a temporary session shows a
  // fixed label instead (SessionBarView's `temporary` branch), so this only
  // gates the persistent path.
  let showTitle = $derived(!isNewSession);
  // The active session is a started temporary one (vs. mere pre-send compose
  // intent, which the composer toggle owns). Drives the fixed bar label.
  let isTemporarySession = $derived(sessionsState.isTemporary && sessionsState.started);
  // Whenever the bar is shown, the navigation button group is shown too (the
  // list/new controls are always reachable), so the only question is whether the
  // bar appears at all: it does once there is context to gauge, a title to show,
  // a started temporary session, or any stored sessions to navigate back to (so
  // a fresh new-chat that has history still shows the list/new controls). A
  // first-ever new-chat with nothing stored stays hidden (its sole action,
  // starting a new session, is already the state).
  let hasSessions = $derived(sessionsState.list.length > 0);
  let showBar = $derived(
    !!messagesState.tokenUsage || showTitle || isTemporarySession || hasSessions,
  );

  // When the bar hides it renders no Bubble, so its ResizeObserver stops
  // reporting; tell the merge owner its width is gone so the CoreBar floats alone
  // again.
  $effect(() => {
    if (!showBar) onWidth?.(0);
  });
</script>

{#snippet bar()}
  <SessionBarView
    tokenUsage={messagesState.tokenUsage ? { used: contextUsed, max: contextMax } : null}
    {showTitle}
    bind:titleText
    {defaultTitle}
    bind:titleInput
    onTitleFocus={handleTitleFocus}
    onTitleBlur={handleTitleBlur}
    onTitleKeydown={handleTitleKeydown}
    generatingTitle={sessionsState.generatingTitle}
    onRegenerateTitle={() => sessionsState.regenerateTitle()}
    temporary={isTemporarySession}
    prevDisabled={!prevSession}
    nextDisabled={!nextSession}
    {isNewSession}
    {confirmingDelete}
    coreDisabled={connectionState.coreUnavailable}
    onList={() => {
      confirmingDelete = false;
      viewState.navigate("sessionList");
    }}
    onPrev={() => prevSession && goToSession(prevSession.id)}
    onNext={() => nextSession && goToSession(nextSession.id)}
    onDelete={handleDeleteClick}
    onNew={() => {
      confirmingDelete = false;
      sessionsState.create();
    }}
    baseColorOverride={themeOverrideHex}
    {merge}
    {onWidth}
  />
{/snippet}

{#if showBar}
  <div class="relative pointer-events-none" style:z-index={zIndex}>
    {#if !onMobile && settingsState.getAlignment() === "center"}
      <!-- Centered: the bar slides down from above the first time a session gains
           content (showBar false -> true). No msgId, so it animates on every such
           entry; the session-restore gate keeps a restored bar from animating on
           load. Off-center (left/right) it just appears, with no entry motion. -->
      <MessageEnter alignment="center" centerDirection="down">
        {@render bar()}
      </MessageEnter>
    {:else}
      {@render bar()}
    {/if}
  </div>
{/if}
