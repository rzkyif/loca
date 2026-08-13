<script lang="ts" module>
  // Plain presentational shape for one session row. The client owns the live
  // session store: it resolves each entry's title (falling back to the creation
  // datetime so nothing reads as "Untitled"), pre-renders the conversation
  // summary into a single string, and flags the current session. This View only
  // renders the rows and emits the raw interaction (open / long-press / delete)
  // back to the client, which decides what loads, what deletes, and how the
  // two-tap delete confirm advances.
  export interface SessionRowView {
    id: string;
    /** Pre-resolved display title (never empty). */
    title: string;
    /** Pre-rendered one-line conversation summary (e.g. role-prefixed snippets),
     *  or empty when there are no messages yet. */
    summary: string;
    /** This is the currently-loaded session: carries the screen-edge `active`
     *  border. */
    active: boolean;
    /** The delete icon is in its armed (second-tap) state for this row. */
    confirmingDelete: boolean;
  }
</script>

<script lang="ts">
  // The session list: a column of floating bubbles on the transparent window
  // background, a header bubble then one bubble per session, using the same
  // bubble look as chat messages. No scroll container of its own; the page's
  // chat scroll area owns scrolling, exactly like the chat view.
  import Bubble from "../primitives/Bubble.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import { bubbleGap, useUiContext } from "../../context.ts";

  const ui = useUiContext();
  let alignment = $derived(ui.getAlignment());
  // On mobile the list fills the screen width (the page wrapper's slim p-3 is the
  // only horizontal padding), so the bubbles go full-width instead of the
  // desktop's fixed 28rem column; the back row is dropped where the OS owns back.
  const mobile = $derived(ui.platform === "mobile");

  let {
    rows = [],
    onSelect = noop,
    onLongPress = noop,
    onDelete = noop,
    onNew = noop,
    onBack = noop,
  }: {
    rows?: SessionRowView[];
    onSelect?: (id: string) => void;
    onLongPress?: (id: string) => void;
    onDelete?: (id: string) => void;
    onNew?: () => void;
    onBack?: () => void;
  } = $props();

  function noop(): void {}
</script>

<div class="flex flex-col" style:gap={bubbleGap(ui)}>
  <!-- Header bubble -->
  <Bubble
    selectedAlignment={alignment}
    size="small"
    fullWidth={mobile}
    extraClass="flex items-center gap-2 {mobile ? '' : 'w-[28rem] max-w-full'}"
  >
    {#if !ui.hasSystemBack}
      <!-- -ml-1 cancels the IconButton's own p-1 inset so the arrow glyph
           left-aligns with the session bubbles' title/summary text. Dropped on
           Android, where the system back returns to chat. -->
      <IconButton
        icon="i-material-symbols-arrow-back-rounded"
        title="Back to Chat"
        size="md"
        variant="subtle"
        class="-ml-1"
        onclick={onBack}
      />
    {/if}
    <h1 class="text-sm font-medium text-default-800 flex-1">Sessions</h1>
    <IconButton
      icon="i-material-symbols-add-rounded"
      title="New Session"
      size="md"
      variant="subtle"
      onclick={onNew}
    />
  </Bubble>

  <!-- Session bubbles -->
  {#if rows.length === 0}
    <Bubble
      selectedAlignment={alignment}
      size="small"
      fullWidth={mobile}
      extraClass="text-sm text-default-500"
    >
      No sessions yet. Start one with the + button above.
    </Bubble>
  {:else}
    {#each rows as row (row.id)}
      <!-- The current session carries the screen-edge `active` border. The
           border paints outside the content div this width class sizes, so
           shave the border's 8px off the content to keep every bubble's
           total width identical (center alignment borders the bottom edge,
           which doesn't affect width). -->
      <Bubble
        selectedAlignment={alignment}
        size="small"
        active={row.active}
        fullWidth={mobile}
        borderColorClass={row.active ? "border-default-400" : ""}
        onclick={() => onSelect(row.id)}
        onlongpress={() => onLongPress(row.id)}
        extraClass="flex items-center gap-3 cursor-pointer {mobile
          ? ''
          : (row.active && alignment !== 'center' ? 'w-[calc(28rem-8px)]' : 'w-[28rem]') +
            ' max-w-full'}"
      >
        <div class="flex flex-col min-w-0 flex-1">
          <span class="text-sm text-default-800 truncate">
            {row.title}
          </span>
          <span class="text-xs text-default-500 truncate">
            {row.summary.length > 0 ? row.summary : "No messages yet"}
          </span>
        </div>
        <IconButton
          icon={row.confirmingDelete
            ? "i-material-symbols-delete-forever-rounded"
            : "i-material-symbols-delete-outline-rounded"}
          title={row.confirmingDelete ? "Confirm delete" : "Delete session"}
          size="md"
          variant="subtle"
          onclick={(e) => {
            e.stopPropagation();
            onDelete(row.id);
          }}
          class={row.confirmingDelete ? "text-accent-red-700" : ""}
        />
      </Bubble>
    {/each}
  {/if}
</div>
