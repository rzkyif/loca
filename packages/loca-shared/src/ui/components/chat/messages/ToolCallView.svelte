<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ToolCallStatus } from "../../../../domain/session.ts";
  import Bubble from "../../primitives/Bubble.svelte";
  import Expandable from "../../primitives/Expandable.svelte";
  import DiffView from "./DiffView.svelte";
  import ErrorDetailView from "./ErrorDetailView.svelte";
  import { useUiContext } from "../../../context.ts";

  // Presentational tool-call bubble: the status-phrase header and the args /
  // result / error / logs blocks. While a tool is awaiting input the bubble
  // reads yellow but stays form-free; the askUser form itself renders in the
  // composer (UserInput). The client wraps this feeding live message + ephemera
  // state; the website feeds scripted state. Alignment and the system-message
  // theme override come from the shared UI context; the agent name and the
  // memory-result markdown renderer are injected, so this stays free of client
  // stores and the markdown pipeline.
  const ui = useUiContext();
  const themeOverrideHex = $derived(ui.systemMessageDefaultColor ?? null);

  type LogLine = { level: string; message: string };

  let {
    toolName,
    status = "completed",
    label = undefined,
    description = undefined,
    args = {},
    result = undefined,
    error = undefined,
    progress = undefined,
    logs = [],
    agentName = "",
    neighborLeft = false,
    neighborRight = false,
    memoryContent = undefined,
    expanded = $bindable(false),
  }: {
    toolName: string;
    status?: ToolCallStatus;
    label?: string;
    description?: string;
    args?: Record<string, unknown>;
    result?: unknown;
    error?: string;
    progress?: number;
    logs?: LogLine[];
    /** Agent name for the status phrase; falls back to "Agent" when empty. */
    agentName?: string;
    neighborLeft?: boolean;
    neighborRight?: boolean;
    /** Renders the `memory_content` result. Client passes MessageMarkdown;
     *  when absent the raw text is shown in a <pre>. */
    memoryContent?: Snippet<[{ title: string; content: string }]>;
    expanded?: boolean;
  } = $props();

  // Aliases so the body below reads against stable names.
  let tcStatus = $derived(status);
  let tcLogs = $derived(logs);
  let tcArgs = $derived(args);

  // Wrapper text around the tool name, keyed by status. When `label` is set the
  // whole sentence is replaced by that label.
  let statusPhrase = $derived.by(() => {
    const agent = (agentName || "").trim() || "Agent";
    switch (tcStatus) {
      case "awaiting_user":
        return { pre: `${agent} awaiting input for `, post: " tool." };
      case "awaiting_permission":
        return { pre: `${agent} awaiting permission for `, post: " tool." };
      case "completed":
        return { pre: `${agent} used `, post: " tool." };
      case "failed":
        return { pre: `${agent} failed to use `, post: " tool." };
      case "cancelled":
        return { pre: `${agent} cancelled `, post: " tool." };
      case "pending":
      case "running":
      default:
        return { pre: `${agent} is using `, post: " tool." };
    }
  });

  let percent = $derived(typeof progress === "number" ? Math.round(progress * 100) : null);
  let isActive = $derived(tcStatus === "pending" || tcStatus === "running");
  let showProgress = $derived(isActive);

  // Well-known result kinds get a dedicated renderer instead of raw JSON: the
  // memory tools return `memory_diff` (before/after) and `memory_content`
  // (full markdown).
  let memoryDiff = $derived.by<{ title: string; before: string; after: string } | null>(() => {
    const r = result as Record<string, unknown> | undefined;
    if (
      !r ||
      typeof r !== "object" ||
      r.kind !== "memory_diff" ||
      typeof r.before !== "string" ||
      typeof r.after !== "string"
    ) {
      return null;
    }
    return {
      title: typeof r.title === "string" ? r.title : "",
      before: r.before,
      after: r.after,
    };
  });
  let memoryResult = $derived.by<{ title: string; content: string } | null>(() => {
    const r = result as Record<string, unknown> | undefined;
    if (
      !r ||
      typeof r !== "object" ||
      r.kind !== "memory_content" ||
      typeof r.content !== "string"
    ) {
      return null;
    }
    return {
      title: typeof r.title === "string" ? r.title : "",
      content: r.content,
    };
  });

  let resultText = $derived.by(() => {
    if (result === undefined) return "";
    try {
      return JSON.stringify(result, null, 2);
    } catch {
      return String(result);
    }
  });

  let argsText = $derived.by(() => {
    try {
      return JSON.stringify(tcArgs ?? {}, null, 2);
    } catch {
      return "";
    }
  });

  let hasArgs = $derived(!!tcArgs && Object.keys(tcArgs).length > 0);
  let hasLogs = $derived(tcLogs.length > 0);
  let hasResult = $derived(tcStatus === "completed" && result !== undefined);
  let hasError = $derived(tcStatus === "failed" && !!error);
  let hasCancelledError = $derived(tcStatus === "cancelled" && !!error);
  let hasBody = $derived(
    !!description || hasArgs || hasLogs || hasResult || hasError || hasCancelledError,
  );

  let awaitingInput = $derived(tcStatus === "awaiting_user" || tcStatus === "awaiting_permission");

  // States that color the whole bubble: a failed call reads red, awaiting input
  // reads yellow. `accent` retints the bubble fill AND every nested `-default-`
  // color (insets, text) to the hue, so the call is themed end to end rather
  // than only at the border. Other states stay on the neutral surface. The
  // yellow flags the call whose askUser/permission prompt is in the composer.
  let accent: "red" | "yellow" | undefined = $derived(
    tcStatus === "failed" ? "red" : awaitingInput ? "yellow" : undefined,
  );

  // Mobile chat bubbles always sit on the agent side (left); desktop follows the
  // window-alignment setting.
  let alignment = $derived(ui.platform === "mobile" ? "left" : ui.getAlignment());
  // Floor the bubble width only while expanded so the body doesn't squish; when
  // collapsed the bubble shrink-wraps the label + description.
  let bubbleExtraClass = $derived(expanded ? "text-default-800 min-w-60" : "text-default-800");
</script>

<div style:display="contents" style:--default-base={themeOverrideHex}>
  <Bubble
    selectedAlignment={alignment}
    size="small"
    {accent}
    extraClass={bubbleExtraClass}
    progress={showProgress ? percent : undefined}
    {neighborLeft}
    {neighborRight}
  >
    <Expandable bind:expanded {alignment} disabled={!hasBody}>
      {#snippet title()}
        <span>
          {#if label}
            {label}
          {:else}
            {statusPhrase.pre}<code
              class="font-mono bg-surface-inset text-default-800 rounded-small px-1.5 py-0.5 text-[0.8em] mx-1"
              >{toolName}</code
            >{statusPhrase.post}
          {/if}
        </span>
      {/snippet}
      {#snippet children()}
        <!-- Body content is intentionally alignment-independent: `text-left`
           overrides the Expandable wrapper's `text-right` so questions, args,
           results, and error/log blocks always read left-to-right. -->
        <div class="flex flex-col gap-2 text-left">
          {#if description}
            <div class="text-xs text-default-600 {alignment === 'right' ? 'text-right' : ''}">
              {description}
            </div>
          {/if}

          {#if hasCancelledError}
            <ErrorDetailView detail={error} />
          {/if}

          <div class="flex flex-col gap-1 text-xs">
            {#if hasArgs}
              <div class="text-default-600">Arguments</div>
              <pre
                class="loca-scroll-inset text-default-800 bg-surface-inset rounded-small px-2 py-1 max-h-32 overflow-auto whitespace-pre">{argsText}</pre>
            {/if}
            {#if hasResult}
              {#if memoryDiff}
                <div class="text-default-600">
                  Changes{memoryDiff.title ? ` to "${memoryDiff.title}"` : ""}
                </div>
                <div class="max-h-48 overflow-auto">
                  <DiffView before={memoryDiff.before} after={memoryDiff.after} />
                </div>
              {:else if memoryResult}
                <div class="text-default-600">
                  {memoryResult.title || "Memory"}
                </div>
                {#if memoryContent}
                  <div
                    class="loca-scroll-inset bg-surface-inset rounded-small px-2 py-1 max-h-48 overflow-auto"
                  >
                    {@render memoryContent(memoryResult)}
                  </div>
                {:else}
                  <pre
                    class="loca-scroll-inset text-default-800 bg-surface-inset rounded-small px-2 py-1 max-h-48 overflow-auto whitespace-pre-wrap">{memoryResult.content}</pre>
                {/if}
              {:else}
                <div class="text-default-600">Result</div>
                <pre
                  class="loca-scroll-inset text-default-800 bg-surface-inset rounded-small px-2 py-1 max-h-48 overflow-auto whitespace-pre">{resultText}</pre>
              {/if}
            {/if}
            {#if hasError}
              <ErrorDetailView detail={error} onAccentSurface />
            {/if}
            {#if hasLogs}
              <div class="text-default-600">Logs</div>
              <div
                class="loca-scroll-inset bg-surface-inset rounded-small px-2 py-1 max-h-32 overflow-auto flex flex-col gap-0.5 whitespace-pre"
              >
                {#each tcLogs as log, i (i)}
                  <div class="text-default-700">
                    <span class="text-default-500">[{log.level}]</span>
                    {log.message}
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        </div>
      {/snippet}
    </Expandable>
  </Bubble>
</div>
