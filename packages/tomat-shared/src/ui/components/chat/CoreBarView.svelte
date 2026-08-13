<script lang="ts" module>
  // The single status a CoreBar paints: the backend CoreStatus merged with the
  // client's transport states (which never ride the wire). The client wrapper
  // computes the merge; the website feeds a static value.
  // While a reconnect is still in flight the transport reads "connecting", then
  // "reconnecting" after a grace period. "disconnected" and "stopped" are the
  // settled states it lands in once retrying has stopped: the core is
  // unreachable, or this device's core simply isn't running. Both are actionable
  // (see onStatusAction) rather than expandable.
  export type DisplayCoreStatus =
    | "starting_up"
    | "downloading"
    | "idle"
    | "busy"
    | "updating"
    | "error"
    | "connecting"
    | "reconnecting"
    | "disconnected"
    | "stopped"
    | "unauthorized";

  type StatusMeta = { icon: string; tone: string; label: string; spin: boolean };

  const STATUS_META: Record<DisplayCoreStatus, StatusMeta> = {
    starting_up: {
      icon: "i-material-symbols-progress-activity",
      tone: "text-default-700",
      label: "Starting up",
      spin: true,
    },
    downloading: {
      icon: "i-material-symbols-downloading",
      tone: "text-accent-blue-700",
      label: "Downloading",
      spin: false,
    },
    idle: {
      icon: "i-material-symbols-check-circle-rounded",
      tone: "text-accent-green-700",
      label: "Ready",
      spin: false,
    },
    busy: {
      icon: "i-material-symbols-progress-activity",
      tone: "text-accent-yellow-700",
      label: "Busy",
      spin: true,
    },
    updating: {
      icon: "i-material-symbols-progress-activity",
      tone: "text-accent-blue-700",
      label: "Updating",
      spin: true,
    },
    error: {
      icon: "i-material-symbols-error-rounded",
      tone: "text-accent-red-700",
      label: "Error",
      spin: false,
    },
    connecting: {
      icon: "i-material-symbols-progress-activity",
      tone: "text-default-700",
      label: "Connecting",
      spin: true,
    },
    reconnecting: {
      icon: "i-material-symbols-progress-activity",
      tone: "text-accent-yellow-700",
      label: "Reconnecting",
      spin: true,
    },
    disconnected: {
      icon: "i-material-symbols-cloud-off-rounded",
      tone: "text-accent-red-700",
      label: "Disconnected",
      spin: false,
    },
    stopped: {
      icon: "i-material-symbols-power-settings-new-rounded",
      tone: "text-default-700",
      label: "Stopped",
      spin: false,
    },
    unauthorized: {
      icon: "i-material-symbols-lock",
      tone: "text-accent-red-700",
      label: "Re-pair needed",
      spin: false,
    },
  };
</script>

<script lang="ts">
  import Bubble from "../primitives/Bubble.svelte";
  import Expand from "../primitives/Expand.svelte";
  import ErrorDetailView from "./messages/ErrorDetailView.svelte";
  import FlushSelect from "../primitives/FlushSelect.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";
  import type { SidecarKind } from "../../../domain/model.ts";
  import type { CoreQueues, SubsystemStatus } from "../../../domain/core-status.ts";
  import { type BubbleMerge, NO_MERGE } from "../../merge.ts";

  // The core bar: a small bubble showing which core the client is connected to,
  // its merged status, and a quick switcher. When the status carries extra
  // detail (a failure, queued work, or something loading) the status pill
  // becomes a toggle that expands a card listing the specifics, folding every
  // sidecar failure into the one core status. All data + the switch / manage /
  // expand actions are owned by the client wrapper and supplied via props; the
  // website feeds static values. Alignment comes from the UI context so both
  // apps match.
  const ui = useUiContext();
  // On mobile the core bar is the chat screen's top app bar: it spans the full
  // width, its status + switcher stay centered, and the Settings entry point
  // lives at the right edge (it is dropped from the composer there).
  const mobile = ui.platform === "mobile";
  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  // User-facing names for the subsystems, per COPY.md (no mechanism words). STT
  // and TTS share the one speech process, so it is a single "Speech" subsystem.
  const SUBSYSTEM_NAMES: Record<SidecarKind, string> = {
    llama: "Language Model",
    "llama-embed": "Search",
    speech: "Speech",
    tool: "Tools",
  };

  let {
    status,
    detail = undefined,
    progress = undefined,
    subsystems = [],
    queues = undefined,
    cores = [],
    currentCoreId = "",
    expanded = false,
    onToggleExpand = undefined,
    onStatusAction = undefined,
    onSwitch,
    onSettings = undefined,
    baseColorOverride = null,
    merge = NO_MERGE,
    onWidth = undefined,
  }: {
    status: DisplayCoreStatus;
    /** Short detail for the tooltip (e.g. "loading whisper", "3 queued"). */
    detail?: string;
    /** 0..1 boot/load progress; appended to the label when starting up. */
    progress?: number;
    /** Per-subsystem breakdown; drives the expanded error / loading list. */
    subsystems?: SubsystemStatus[];
    /** Queue / in-flight counts; drives the expanded busy list. */
    queues?: CoreQueues;
    cores?: { id: string; name: string }[];
    currentCoreId?: string;
    /** Whether the detail card is open (controlled by the wrapper's registry). */
    expanded?: boolean;
    /** Toggle the detail card; only wired when there is detail to show. */
    onToggleExpand?: () => void;
    /** Recover from a settled failure: retry the connection when disconnected,
     *  start this device's core when stopped. Only these two states offer it. */
    onStatusAction?: () => void;
    onSwitch?: (id: string) => void;
    /** Mobile only: opens Settings from the top app bar (the gear is dropped
     *  from the composer there). Omitted on desktop. */
    onSettings?: () => void;
    baseColorOverride?: string | null;
    /** How this bar merges into the bubble above it (the SessionBar in chat, the
     *  Settings panel in the settings view); defaults to floating alone. */
    merge?: BubbleMerge;
    /** Report the bar's rendered width so a merging neighbor can size the seam. */
    onWidth?: (width: number) => void;
  } = $props();

  const meta = $derived(STATUS_META[status]);
  const pct = $derived(
    (status === "starting_up" || status === "downloading") && typeof progress === "number"
      ? ` ${Math.round(progress * 100)}%`
      : "",
  );
  const options = $derived(cores.map((c) => ({ value: c.id, label: c.name })));

  const erroredSubsystems = $derived(subsystems.filter((s) => s.status === "Error"));
  const loadingSubsystems = $derived(subsystems.filter((s) => s.status === "Loading"));
  // The lines shown while busy: each queue that has work, plus the active turns.
  const busyLines = $derived.by<string[]>(() => {
    if (!queues) return [];
    const lines: string[] = [];
    if (queues.llmActive > 0 || queues.llmQueued > 0) {
      lines.push(`Language Model: ${queues.llmActive} working, ${queues.llmQueued} waiting`);
    }
    if (queues.speechActive > 0 || queues.speechQueued > 0) {
      lines.push(`Speech: ${queues.speechActive} working, ${queues.speechQueued} waiting`);
    }
    if (queues.activeStreams > 0) lines.push(`Active sessions: ${queues.activeStreams}`);
    return lines;
  });

  // Only the states that actually carry specifics are expandable: a failure, a
  // busy queue, or something loading. Everything else paints a plain pill.
  const hasDetail = $derived(
    (status === "error" && erroredSubsystems.length > 0) ||
      (status === "busy" && busyLines.length > 0) ||
      (status === "starting_up" && loadingSubsystems.length > 0),
  );
  const open = $derived(expanded && hasDetail);
  // The two settled failures offer a way out instead of a detail card, so the
  // pill clicks through to onStatusAction and grows no chevron.
  const isActionable = $derived(!hasDetail && (status === "disconnected" || status === "stopped"));
  // Name the recovery in the tooltip; the label alone only names the problem.
  const actionTitle = $derived(
    !isActionable ? undefined : status === "stopped" ? "Start Core" : "Connect again",
  );

  function subsystemName(kind: SidecarKind): string {
    return SUBSYSTEM_NAMES[kind] ?? kind;
  }
</script>

{#snippet pillInner()}
  <i class="flex text-base {meta.icon} {meta.spin ? 'animate-spin' : ''}"></i>
  <span class="text-sm whitespace-nowrap">{meta.label}{pct}</span>
  {#if hasDetail}
    <i
      class="flex text-base i-material-symbols-chevron-right-rounded transition-transform duration-200 {open
        ? 'rotate-90'
        : ''}"
    ></i>
  {/if}
{/snippet}

<div style:display="contents" style:--default-base={baseColorOverride}>
  <Bubble
    selectedAlignment={ui.getAlignment()}
    fullWidth={mobile}
    size="small"
    extraClass="flex flex-col gap-2"
    flatCorners={merge.flatCorners}
    overlapTop={merge.overlapTop}
    {onWidth}
  >
    {#if mobile}
      <!-- Mobile top app bar, three screen-anchored regions: the core picker (an
           icon-button dropdown) at the left edge, the status pill centered across
           the FULL bar width (equal 1fr side columns, so it stays screen-centered
           regardless of the side controls' widths), and Settings at the right
           edge. -->
      <div class="grid grid-cols-[1fr_auto_1fr] items-center gap-2 w-full">
        <div class="flex items-center justify-self-start">
          {@render corePicker()}
        </div>
        <div class="flex items-center justify-self-center min-w-0">
          {@render statusPill()}
        </div>
        <div class="flex items-center justify-self-end">
          {#if onSettings}
            <IconButton
              icon="i-material-symbols-settings-outline-rounded"
              title="Settings"
              size="lg"
              variant="subtle"
              surface="circle"
              class="shrink-0"
              onclick={() => onSettings?.()}
            />
          {/if}
        </div>
      </div>
    {:else}
      <!-- Header row. Collapsed it packs left; expanded the status pill stays left
           and the switcher moves to the right edge (justify-between over the card's
           width below). -->
      <div class="flex items-center gap-2" class:w-full={open} class:justify-between={open}>
        {@render statusPill()}
        <div class="flex items-center gap-2 min-w-0">
          {@render corePicker()}
        </div>
      </div>
    {/if}

    <!-- Detail card: matches the system-prompt / thinking bubble inset card. -->
    {#if hasDetail}
      <Expand {open}>
        <div
          class="bg-surface-inset px-4 py-2 rounded-large text-default-700 text-xs text-left flex flex-col gap-2"
        >
          {#if status === "error"}
            {#each erroredSubsystems as s (s.kind)}
              <ErrorDetailView message={subsystemName(s.kind)} detail={s.message} />
            {/each}
          {:else if status === "busy"}
            {#each busyLines as line (line)}
              <div>{line}</div>
            {/each}
          {:else if status === "starting_up"}
            {#each loadingSubsystems as s (s.kind)}
              <div>{subsystemName(s.kind)} is loading</div>
            {/each}
          {/if}
        </div>
      </Expand>
    {/if}
  </Bubble>
</div>

<!-- Connected-core status pill. An inset pill (like the SessionBar gauge) so the
     bar's regions read as solid controls rather than floating text. The tone
     colors the icon and label by status, at the SessionBar text lightness (the
     -700 shade). When there is detail it becomes a toggle; when the status is a
     settled failure it becomes the way out of it. Defined at the top level (not
     inside <Bubble>) so it is a local snippet, not a snippet prop. -->
{#snippet statusPill()}
  {#if hasDetail || isActionable}
    <button
      type="button"
      data-region="core-status"
      title={actionTitle ?? detail ?? meta.label}
      aria-expanded={hasDetail ? open : undefined}
      onclick={() => (hasDetail ? onToggleExpand?.() : onStatusAction?.())}
      class="flex items-center gap-1.5 shrink-0 h-8 px-3 bg-surface-inset rounded-large select-none transition-interactive hov:bg-surface-inset-strong hov:cursor-pointer {meta.tone}"
      use:ripple={{ durationMs: rippleDuration }}
    >
      {@render pillInner()}
    </button>
  {:else}
    <span
      data-region="core-status"
      class="flex items-center gap-1.5 shrink-0 h-8 px-3 bg-surface-inset rounded-large select-none {meta.tone}"
      title={detail ?? meta.label}
    >
      {@render pillInner()}
    </span>
  {/if}
{/snippet}

<!-- Quick core switcher. On desktop it is a labelled inset pill (shows the
     connected core's name, matching the SessionBar title field). On mobile it is
     an icon-button dropdown at the left edge of the app bar (an icon that opens
     the same native picker on tap), matching the Settings gear opposite it. -->
{#snippet corePicker()}
  {#if mobile}
    <FlushSelect
      value={currentCoreId}
      {options}
      onchange={(v) => onSwitch?.(v)}
      ariaLabel="Connected core"
      title="Switch core"
      icon="i-material-symbols-hub-outline"
      iconOnly
      rounded="rounded-large"
      textClass="text-default-700"
      class="p-2 text-xl bg-surface-inset hov:bg-surface-inset-strong"
    />
  {:else}
    <FlushSelect
      value={currentCoreId}
      {options}
      onchange={(v) => onSwitch?.(v)}
      ariaLabel="Connected core"
      title="Switch core"
      rounded="rounded-large"
      textClass="text-default-700 hov:text-default-900"
      class="h-8 px-3 bg-surface-inset"
    />
  {/if}
{/snippet}
