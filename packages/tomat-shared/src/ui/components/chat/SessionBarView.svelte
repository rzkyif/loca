<script lang="ts">
  import Bubble from "../primitives/Bubble.svelte";
  import ButtonGroup from "../primitives/ButtonGroup.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import { useUiContext } from "../../context.ts";
  import { type BubbleMerge, NO_MERGE } from "../../merge.ts";

  // The session bar: a small bubble carrying the context-usage gauge, the
  // (editable) session title, and session navigation. All behaviour (title
  // editing, regenerate, delete-confirm, navigation) is owned by the client
  // wrapper and supplied via props/binds; the website feeds static values.
  // Alignment comes from the UI context so both apps match. (Which core you're
  // on is shown by the CoreBar, not here.)
  const ui = useUiContext();
  // Mobile pares the session controls down to the two that matter on a phone:
  // open the session list and start a new session. Prev / next paging and the
  // inline delete are dropped (the list view owns delete via long-press).
  const mobile = $derived(ui.platform === "mobile");

  let {
    tokenUsage = null,
    showTitle = false,
    titleText = $bindable(""),
    defaultTitle = "",
    titleInput = $bindable(),
    onTitleFocus,
    onTitleBlur,
    onTitleKeydown,
    generatingTitle = false,
    onRegenerateTitle,
    temporary = false,
    prevDisabled = false,
    nextDisabled = false,
    isNewSession = false,
    confirmingDelete = false,
    coreDisabled = false,
    onList,
    onPrev,
    onNext,
    onDelete,
    onNew,
    baseColorOverride = null,
    merge = NO_MERGE,
    onWidth = undefined,
  }: {
    /** Context-window usage; null hides the gauge. */
    tokenUsage?: { used: number; max: number } | null;
    showTitle?: boolean;
    titleText?: string;
    defaultTitle?: string;
    titleInput?: HTMLInputElement;
    onTitleFocus?: () => void;
    onTitleBlur?: () => void;
    onTitleKeydown?: (e: KeyboardEvent) => void;
    generatingTitle?: boolean;
    onRegenerateTitle?: () => void;
    /** Temporary (RAM-only) session: the title is a fixed, non-editable
     *  "Temporary Session" label with no regenerate button, signalling that
     *  the chat is never saved. */
    temporary?: boolean;
    prevDisabled?: boolean;
    nextDisabled?: boolean;
    isNewSession?: boolean;
    confirmingDelete?: boolean;
    /** The core can't serve a request right now. Every control on this bar reads
     *  or writes sessions on the core, so all of them go inert together rather
     *  than failing one by one when clicked. */
    coreDisabled?: boolean;
    onList?: () => void;
    onPrev?: () => void;
    onNext?: () => void;
    onDelete?: () => void;
    onNew?: () => void;
    /** Per-surface base-color override hex (appearance.sessionBarDefaultColor). */
    baseColorOverride?: string | null;
    /** How this bar merges with the CoreBar (mobile: CoreBar sits above it and it
     *  is the lower bubble; desktop: CoreBar sits below it); defaults to none. */
    merge?: BubbleMerge;
    /** Report the bar's rendered width so a merging neighbor can size the seam. */
    onWidth?: (width: number) => void;
  } = $props();

  const contextRatio = $derived(
    tokenUsage && tokenUsage.max > 0 ? tokenUsage.used / tokenUsage.max : 0,
  );
  const contextColor = $derived(
    contextRatio < 0.5
      ? "bg-accent-green-200"
      : contextRatio < 0.9
        ? "bg-accent-yellow-200"
        : "bg-accent-red-200",
  );

  function formatTokens(n: number): string {
    if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
    return n.toString();
  }
</script>

<div style:display="contents" style:--default-base={baseColorOverride}>
  <Bubble
    selectedAlignment={ui.getAlignment()}
    fullWidth={mobile}
    size="small"
    extraClass="flex items-center gap-2"
    flatCorners={merge.flatCorners}
    overlapTop={merge.overlapTop}
    {onWidth}
  >
    {#if tokenUsage}
      <div
        data-region="context-gauge"
        class="relative w-12 h-8 bg-surface-inset rounded-large overflow-hidden shrink-0 border-0.25em border-default-200"
        title="Context: {formatTokens(tokenUsage.used)} / {formatTokens(tokenUsage.max)}"
      >
        <div
          class="{contextColor} w-full absolute bottom-0 transition-all duration-300"
          style="height: {Math.min(contextRatio * 100, 100)}%"
        ></div>
        <span
          class="absolute inset-0 flex items-center justify-center text-xs font-medium text-default-700 leading-none"
        >
          {Math.round(contextRatio * 100)}%
        </span>
      </div>
    {/if}

    {#if temporary}
      <!-- Temporary session: a fixed, non-editable label (no input, no
           regenerate) so it reads as a chat that is never saved. -->
      <div
        class="flex items-center gap-1.5 min-w-0 h-8 px-3 overflow-hidden bg-surface-inset rounded-large text-sm text-default-700"
        class:flex-1={mobile}
      >
        <i class="flex i-material-symbols-timer-outline-rounded shrink-0"></i>
        <span class="truncate">Temporary Session</span>
      </div>
    {:else if showTitle}
      <!-- Title (grid overlap technique for auto-sizing). The container is the
           only `min-w-0` flex item, so it absorbs the squeeze when the bubble
           hits its max width; the invisible span sizes it and the input shows an
           ellipsis when blurred. On mobile the bar is full-width, so the title
           grows to fill the row (flex-1) and its input spans the space (w-full),
           letting long titles use the whole width instead of hugging content. -->
      <div
        class="tomat-focus-wrap flex items-center min-w-0 h-8 overflow-hidden bg-surface-inset rounded-large text-sm"
        class:flex-1={mobile}
      >
        <div class="grid items-center min-w-0 overflow-hidden" class:w-full={mobile}>
          <span
            class="invisible row-start-1 col-start-1 whitespace-pre pl-3 pr-1 py-1"
            aria-hidden="true">{titleText || defaultTitle}</span
          >
          <input
            size="1"
            aria-label="Session Title"
            bind:this={titleInput}
            bind:value={titleText}
            onfocus={() => onTitleFocus?.()}
            onblur={() => onTitleBlur?.()}
            onkeydown={(e) => onTitleKeydown?.(e)}
            placeholder={defaultTitle}
            disabled={coreDisabled}
            class="row-start-1 col-start-1 w-full min-h-full pl-3 pr-1 py-1 text-default-700 flex items-center text-ellipsis"
          />
        </div>
        <IconButton
          icon={generatingTitle
            ? "i-material-symbols-progress-activity animate-spin"
            : "i-material-symbols-auto-awesome-rounded"}
          title={generatingTitle ? "Generating Title…" : "Regenerate Title"}
          size="sm"
          disabled={generatingTitle || coreDisabled}
          class="mr-2"
          onclick={() => onRegenerateTitle?.()}
        />
      </div>
    {/if}

    <!-- ml-auto keeps the controls pinned to the right edge of the full-width
         mobile bar even when there is no title to push them there (a new/unsaved
         session). -->
    <ButtonGroup size="sm" class={mobile ? "shrink-0 ml-auto" : "shrink-0"}>
      <IconButton
        icon="i-material-symbols-format-list-bulleted-rounded"
        title="Session List"
        size="sm"
        disabled={coreDisabled}
        onclick={() => onList?.()}
      />
      {#if !mobile}
        <IconButton
          icon="i-material-symbols-chevron-left-rounded"
          title="Previous Session"
          size="sm"
          disabled={prevDisabled || coreDisabled}
          onclick={() => onPrev?.()}
        />
        <IconButton
          icon="i-material-symbols-chevron-right-rounded"
          title="Next Session"
          size="sm"
          disabled={nextDisabled || coreDisabled}
          onclick={() => onNext?.()}
        />
      {/if}
      {#if !isNewSession && !mobile}
        <IconButton
          icon={confirmingDelete
            ? "i-material-symbols-delete-forever-rounded"
            : "i-material-symbols-delete-outline-rounded"}
          title={confirmingDelete ? "Confirm Delete" : "Delete Session"}
          size="sm"
          disabled={coreDisabled}
          onclick={() => onDelete?.()}
          data-delete-btn
        />
        <IconButton
          icon="i-material-symbols-add-rounded"
          title="New Session"
          size="sm"
          disabled={coreDisabled}
          onclick={() => onNew?.()}
        />
      {/if}
    </ButtonGroup>
  </Bubble>
</div>
