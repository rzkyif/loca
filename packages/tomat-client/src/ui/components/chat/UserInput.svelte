<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import type { Alignment, Monitor, Attachment, MessagePart } from "$lib/util/types";
  import { platform } from "$lib/platform";
  import { useUiContext } from "@tomat/shared/ui/context";
  import type { PendingPermission } from "../../state";
  import {
    downloadsState,
    messagesState,
    permissionState,
    scheduleConfirmState,
    memoriesState,
    mcpState,
    serversState,
    sessionsState,
    settingsState,
    snippetsState,
    streamingState,
    viewState,
  } from "../../state";
  import { connectionState } from "$stores/connection.svelte";
  import { getLogger } from "$lib/util/log";

  const log = getLogger("user-input");
  const attachLog = getLogger("attach");
  const uiLog = getLogger("ui");

  const ui = useUiContext();
  const onMobile = ui.platform === "mobile";

  async function sendMessages(_anchorUserId?: string): Promise<void> {
    // Trigger the server-side chat turn. The streaming state subscribes
    // to chat.* frames and mutates the message list as content arrives.
    streamingState.beginTurn(_anchorUserId ?? null);
    streamingState.start();
  }

  // Edit / reprocess flows in messagesState regenerate a turn by calling
  // back into this dispatch (see the handler note in messages.svelte.ts).
  messagesState.setLLMHandlers(sendMessages);

  import { applySnippets, snippetTrigger } from "$lib/snippets/snippets";
  import {
    type AutocompleteOption,
    collectExistingTriggers,
    useAutocomplete,
  } from "$composables/use-autocomplete.svelte";
  import { useStt } from "$composables/use-stt-input.svelte";
  import { memoryTrigger } from "$stores/memories.svelte";
  import {
    applySystemPromptOverride,
    buildContextBlock,
    buildSystemPromptBase,
  } from "$lib/prompts/system-prompt";
  import SnippetAutocomplete from "./SnippetAutocomplete.svelte";
  import { ComposerAttachments } from "$composables/use-composer-attachments.svelte";
  import { InputShortcuts } from "$composables/use-input-shortcuts.svelte";
  import { PromptModes } from "$composables/use-prompt-modes.svelte";
  import { AskUser, type PendingAsk } from "$composables/use-askuser.svelte";
  import { McpPromptArgs } from "$composables/use-mcp-prompt-args.svelte";
  import { vadManager } from "$stores/vad.svelte";
  import { shortcutHandler } from "$stores/shortcut.svelte";
  import { useBlink } from "$composables/use-blink.svelte";
  import type { ActivationMode } from "@tomat/shared";
  import AttachmentList from "./AttachmentList.svelte";
  import QuickModelBar from "./userinput/QuickModelBar.svelte";
  import AutocorrectAlert from "./userinput/AutocorrectAlert.svelte";
  import ScheduleConfirmForm from "./userinput/ScheduleConfirmForm.svelte";
  import UserInputView from "@tomat/shared/ui/components/chat/UserInputView.svelte";
  import PromptButtonsView from "@tomat/shared/ui/components/chat/userinput/PromptButtonsView.svelte";
  import Modal from "@tomat/shared/ui/components/primitives/Modal.svelte";
  import { hasAlpha } from "$lib/appearance/color";

  const themeOverride = $derived(
    settingsState.currentSettings["appearance.userInputDefaultColor"] as string,
  );
  const themeOverrideHex = $derived(hasAlpha(themeOverride) ? themeOverride : null);

  let text = $state("");
  // Arguments the user filled in for a `/prompt` MCP reference, keyed by prompt
  // name. Populated when an argument-taking prompt is picked from the "/" menu
  // (the inline form), read at send time to resolve the prompt, then cleared.
  let mcpPromptArgs = $state<Record<string, Record<string, string>>>({});
  let monitors: Monitor[] = $state([]);
  let textareaElement: HTMLTextAreaElement | undefined = $state();
  let mirrorSpan: HTMLSpanElement | undefined = $state();

  // The composer's attachment pipeline (pending list, pickers, paste, capture,
  // image preview) is its own stateful unit; this component owns the lifecycle
  // (monitor fetch in onMount) and the `attachmentParts` derived, and supplies
  // the refocus hook.
  const composer = new ComposerAttachments({
    onMobile,
    focus: () => focusInput(),
  });

  // The `@trigger` autocomplete dropdown and the speech-to-text pipeline are
  // each their own stateful unit; this component owns the lifecycle wiring (see
  // onMount / the effects below) and the option list, which spans two stores.
  const ac = useAutocomplete();
  ac.bind(
    () => textareaElement,
    () => mirrorSpan,
  );
  const stt = useStt();

  // OS-level input shortcuts (attach/capture), armed only while mounted and the
  // window is visible; this component drives register/clear from its visibility
  // subscription in onMount. The schedule-confirm prompt's editable draft lives
  // in its own unit; this component keeps the $derived pending reads and the
  // $effect that mirrors the pending frame into the draft.
  const inputShortcuts = new InputShortcuts();
  const prompt = new PromptModes();
  // The askUser prompt's editable answers live in their own unit; this
  // component keeps the $derived pending read (off the awaiting tool message's
  // ephemera) and the $effect that mirrors it in.
  const ask = new AskUser();
  // The inline argument form for an argument-taking "/prompt" picked from the
  // menu, rendered through the same AskUserFormView a tool's askUser uses.
  const argMode = new McpPromptArgs();

  let autocompleteOptions = $derived.by<AutocompleteOption[]>(() => {
    if (!ac.open) return [];
    const prefix = ac.prefix.toLowerCase();
    const existing = collectExistingTriggers(text, ac.triggerStart, ac.triggerEnd);
    // The leading symbol the user typed (`#`/`@`/`/`) picks the source list:
    // snippets match by their full trigger so only the right symbol passes;
    // memories are `@`-only references.
    const snippets = snippetsState.snippets
      .map((s) => ({ snippet: s, trigger: snippetTrigger(s) }))
      .filter(({ snippet, trigger }) => {
        if (!trigger.toLowerCase().startsWith(prefix)) return false;
        // insert-user snippets are explicitly designed to be dropped inline
        // multiple times, so don't filter them out even if already present.
        if (snippet.placement !== "insert-user" && existing.has(trigger.toLowerCase())) {
          return false;
        }
        return true;
      })
      .map<AutocompleteOption>(({ snippet, trigger }) => ({
        id: `snippet:${snippet.id}`,
        name: snippet.name,
        trigger,
        source: "snippet",
      }));
    const snippetTriggers = new Set(
      snippetsState.snippets.map((s) => snippetTrigger(s).toLowerCase()),
    );
    const memories = memoriesState.memories
      .filter((d) => d.enabled)
      .map((d) => ({ memory: d, trigger: memoryTrigger(d) }))
      .filter(
        ({ trigger }) =>
          trigger.toLowerCase().startsWith(prefix) &&
          !existing.has(trigger.toLowerCase()) &&
          !snippetTriggers.has(trigger.toLowerCase()),
      )
      .sort((a, b) => a.trigger.localeCompare(b.trigger))
      .map<AutocompleteOption>(({ memory, trigger }) => ({
        id: `memory:${memory.id}`,
        name: memory.title,
        trigger,
        source: "memory",
      }));
    // MCP prompts trigger with "/" (enabled ones only); MCP resources are
    // "@"-referenceable. Both resolve core-side at send.
    const mcpResSlug = (n: string) =>
      `@${n
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, "")}`;
    const mcpPrompts = mcpState.prompts
      .filter((p) => p.enabled)
      .map((p) => ({ p, trigger: `/${p.name}` }))
      .filter(
        ({ trigger }) =>
          trigger.toLowerCase().startsWith(prefix) && !existing.has(trigger.toLowerCase()),
      )
      .map<AutocompleteOption>(({ p, trigger }) => ({
        id: `mcp_prompt:${p.serverId}:${p.name}`,
        name: p.serverName,
        trigger,
        source: "mcp_prompt",
      }));
    const mcpResources = mcpState.resources
      .map((r) => ({ r, trigger: mcpResSlug(r.name) }))
      .filter(
        ({ trigger }) =>
          trigger.toLowerCase().startsWith(prefix) &&
          !existing.has(trigger.toLowerCase()) &&
          !snippetTriggers.has(trigger.toLowerCase()),
      )
      .map<AutocompleteOption>(({ r, trigger }) => ({
        id: `resource:${r.serverId}:${r.uri}`,
        name: r.name,
        trigger,
        source: "resource",
      }));
    return [...snippets, ...memories, ...mcpPrompts, ...mcpResources];
  });

  $effect(() => {
    ac.clampIndex(autocompleteOptions.length);
  });

  // Transient status notice shown through the textarea placeholder. Chat
  // mode has no room for banners or badges: short, plain-language messages
  // ("Transcription failed! Please try again.") surface where the user is
  // already looking, then clear on their own. Details go to the logs.
  let inputNotice = $state<string | null>(null);
  let inputNoticeTimeout: ReturnType<typeof setTimeout> | null = null;

  function showInputNotice(message: string, ms = 5000) {
    inputNotice = message;
    if (inputNoticeTimeout) clearTimeout(inputNoticeTimeout);
    inputNoticeTimeout = setTimeout(() => {
      inputNotice = null;
    }, ms);
  }

  let llmStatus = $derived(serversState.serverStatuses.llama.status);

  // Pulses the gear yellow while there are pending startup downloads and
  // Settings is closed. Yellow sits at the 700/900 lightness levels so it
  // matches the adjacent default-700 icon buttons, just yellow-tinted. Interval
  // matches the IconButton's duration-500 so the color is always mid-tween. See
  // useBlink.
  const settingsBlink = useBlink();
  $effect(() => settingsBlink.run(downloadsState.hasPending));
  const gearTone = $derived(
    downloadsState.hasPending
      ? settingsBlink.on
        ? "text-accent-yellow-900"
        : "text-accent-yellow-700"
      : undefined,
  );
  // True whenever the user has something to interrupt: either an LLM stream
  // or an active tool call (running or awaiting input). Drives the stop button
  // so tool calls can be aborted the same way streams are.
  let hasActiveWork = $derived(streamingState.hasActiveWork);

  // Permission mode: a running tool is paused on a permission it was not
  // always-allowed. The text input area shows the request instead of the
  // textarea; the voice/send controls become Deny / Allow. The accept applies
  // to this tool call only ("always allow" lives in the Extension detail view).
  let permissionRequest = $derived(permissionState.pending);

  // Map the pending permission to the plain-language action + concrete target
  // the shared composer renders. Kept here (not in the View) because the kind ->
  // sentence mapping is client domain logic; the View takes the resolved strings.
  function permissionParts(p: PendingPermission): { action: string; detail?: string } {
    switch (p.permissionKind) {
      case "net":
        return { action: "connect to a server", detail: p.resource };
      case "read":
        return { action: "read a file", detail: p.resource };
      case "write":
        return { action: "write to a file", detail: p.resource };
      case "run":
        return { action: "run a program", detail: p.resource };
      case "env":
        return p.resource
          ? { action: "read an environment variable", detail: p.resource }
          : { action: "read all environment variables" };
      case "ffi":
        return p.resource
          ? { action: "load a native library", detail: p.resource }
          : { action: "load native libraries" };
      case "sys":
        return { action: "read system information", detail: p.resource || undefined };
      case "memories":
        return p.resource === "write"
          ? { action: "create and edit your memories" }
          : { action: "read your memories" };
      case "llm":
        return { action: "use the language model" };
      case "tts":
        return { action: "speak text aloud" };
      case "stt":
        return { action: "transcribe audio to text" };
    }
  }

  let permissionPrompt = $derived.by(() => {
    if (!permissionRequest) return null;
    const parts = permissionParts(permissionRequest);
    return {
      toolName: permissionRequest.toolName,
      action: parts.action,
      detail: parts.detail,
      declared: permissionRequest.declared,
    };
  });

  // askUser mode: a running tool called `ctx.askUser()` and is paused on its
  // form. The request lives in the awaiting tool message's ephemera, so there is
  // no dedicated store; derive the one pending form and mirror it into the unit.
  // If several tools await at once, surface the oldest first (FIFO, matching the
  // permission queue): `messages` is newest-first, so scan from the tail.
  let pendingAsk = $derived.by<PendingAsk | null>(() => {
    const msgs = messagesState.messages;
    for (let i = msgs.length - 1; i >= 0; i--) {
      const m = msgs[i];
      if (
        m.role === "tool" &&
        m.status === "awaiting_user" &&
        m.callId &&
        m.ephemera?.askUser &&
        m.ephemera.askUser.answers === null
      ) {
        return {
          callId: m.callId,
          requestId: m.ephemera.askUser.requestId,
          questions: m.ephemera.askUser.questions,
        };
      }
    }
    return null;
  });
  $effect(() => ask.sync(pendingAsk));

  // Schedule-confirm mode: a running tool proposed a scheduled prompt and is
  // paused on this editable form. Mirrors the permission mode: the textarea
  // is replaced by the form, attach/voice/send controls hide, X and check
  // buttons decide. The user's edits ride back on the accept.
  let scheduleConfirm = $derived(scheduleConfirmState.pending);
  $effect(() => {
    // The pending frame is reactive deep state, so its draft is a Proxy;
    // structuredClone would throw DataCloneError on it. $state.snapshot
    // returns a plain editable clone (the same call the accept path uses).
    prompt.scheduleDraft = scheduleConfirm ? $state.snapshot(scheduleConfirm.draft) : null;
  });

  let hasContent = $derived(text.trim().length > 0 || composer.attachments.length > 0);

  let placeholderText = $derived(
    inputNotice
      ? inputNotice
      : connectionState.unavailableCopy
        ? connectionState.unavailableCopy
        : downloadsState.hasPending
          ? "Pending download, open settings!"
          : downloadsState.loading
            ? "Connecting to Core..."
            : stt.processing
              ? "Transcribing..."
              : vadManager.enabled && vadManager.listening
                ? "Listening..."
                : vadManager.enabled
                  ? "Waiting for speech..."
                  : llmStatus === "Error"
                    ? "Failed to start LLM server!!"
                    : llmStatus === "Loading"
                      ? "Waiting for LLM server..."
                      : "Enter your instructions...",
  );
  let sttStatus = $derived(serversState.serverStatuses.speech.status);

  let attachmentParts = $derived(
    composer.attachments.map((att): MessagePart => {
      if (att.type === "image") {
        return {
          type: "image_url",
          image_url: {
            url: `data:${att.mime || "image/png"};base64,${att.pendingData}`,
          },
        };
      }
      return {
        type: "document",
        filename: att.filename,
        markdown: att.pendingData,
      };
    }),
  );

  // Resolve every `/prompt` MCP reference in the sent text into an instruction
  // block, fetched live from the server with any collected arguments. The blocks
  // are folded into the turn's system prompt so they are both sent to the model
  // and visible in the system-prompt bubble. Unknown/disabled `/tokens` are left
  // alone (they stay as literal text in the user's message).
  async function resolveMcpPromptBlock(
    userText: string,
    promptArgs: Record<string, Record<string, string>>,
  ): Promise<string | null> {
    const enabled = mcpState.prompts.filter((p) => p.enabled);
    if (enabled.length === 0) return null;
    const byName = new Map(enabled.map((p) => [p.name.toLowerCase(), p]));
    const seen = new Set<string>();
    const blocks: string[] = [];
    for (const m of userText.matchAll(/(?:^|[^\w@#/])\/([A-Za-z0-9_-]+)/g)) {
      const token = m[1].toLowerCase();
      if (seen.has(token)) continue;
      seen.add(token);
      const p = byName.get(token);
      if (!p) continue;
      try {
        const args = promptArgs[p.name] ?? {};
        const resolved = await mcpState.resolvePrompt(p.serverId, p.name, args);
        if (resolved.trim()) {
          blocks.push(
            `[Prompt /${p.name} from ${p.serverName} - follow these instructions]\n${resolved}`,
          );
        }
      } catch (err) {
        uiLog.warn(`MCP prompt /${p.name} failed to resolve:`, err);
        showInputNotice(`Couldn't resolve /${p.name} from ${p.serverName}.`);
      }
    }
    return blocks.length > 0 ? blocks.join("\n\n") : null;
  }

  function applySnippetTrigger(option: { trigger: string }) {
    const ta = textareaElement;
    const result = ac.applyTrigger(text, option.trigger);
    if (!result) return;
    text = result.text;
    const caret = result.caret;
    // Restore caret position after Svelte updates the DOM value.
    queueMicrotask(() => {
      ta?.focus({ preventScroll: true });
      ta?.setSelectionRange(caret, caret);
    });
  }

  // Autocomplete pick: insert the trigger, then - for an MCP prompt that takes
  // arguments - open the inline argument form. The token is inserted first (so
  // the autocomplete state is still fresh); submitting the form stashes the args
  // and sends the turn straight away (see the begin callback below).
  function onAutocompleteSelect(option: { id: string; trigger: string }) {
    // Recover the source-carrying option BEFORE inserting the trigger: the
    // autocomplete View types its option as `{ id, name, trigger }`, and
    // `applySnippetTrigger` closes the dropdown, which empties `autocompleteOptions`
    // (it early-returns `[]` when closed). Looking it up first is what lets an MCP
    // prompt with arguments open the form.
    const full = autocompleteOptions.find((o) => o.id === option.id);
    applySnippetTrigger(option);
    if (full?.source !== "mcp_prompt") return;
    const prompt = mcpState.prompts.find((p) => `/${p.name}` === option.trigger);
    if (!prompt || prompt.arguments.length === 0) return;
    // Submitting the form sends the turn immediately rather than handing the
    // composer back for more editing (which invites a half-filled, ambiguous
    // state). The `/prompt` token is already in the text from applySnippetTrigger
    // above, so resolveMcpPromptBlock picks it up with these stashed args.
    argMode.begin(prompt, (p, args) => {
      mcpPromptArgs = { ...mcpPromptArgs, [p.name]: args };
      void handleSend();
    });
  }

  function handleCompositionStart() {
    ac.onCompositionStart();
  }

  function handleCompositionEnd() {
    ac.onCompositionEnd(text);
  }

  function handleTextInput() {
    if (stt.showDiff) stt.clearDiff();
    ac.updateFromInput(text);
  }

  function handleSelectionChange() {
    if (ac.open) ac.updateFromInput(text);
  }

  // Programmatic focus is suppressed on mobile: auto-focusing pops the soft
  // keyboard without the user asking, covering half the screen. The user taps
  // the composer to focus instead. (User-initiated focus, e.g. tapping Edit,
  // goes through the exported focus() / direct calls and is unaffected.)
  //
  // `preventScroll` is load-bearing: this fires on mount, so when entering chat
  // from another panel the composer is auto-focused WHILE the panel layer is
  // still translated off-screen mid-slide. Without it the WebView scrolls the
  // (overflow-hidden but still programmatically scrollable) layer to bring the
  // off-screen textarea into view, shifting the whole UI by ~a viewport and then
  // snapping back when the scroll resets - the "slides to the wrong spot then
  // teleports" glitch.
  function focusTextarea() {
    if (onMobile) return;
    if (messagesState.messages.length == 0 && textareaElement) {
      setTimeout(() => textareaElement?.focus({ preventScroll: true }), 0);
    }
  }

  function focusInput() {
    if (onMobile) return;
    if (textareaElement) setTimeout(() => textareaElement?.focus({ preventScroll: true }), 0);
  }

  $effect(() => {
    focusTextarea();
  });

  onMount(() => {
    const cleanups: Array<() => void> = [];

    // Input-mode shortcuts (attach/capture) are registered on the OS level via
    // tauri-plugin-global-shortcut, but only while:
    //   (1) UserInput is mounted (i.e. Settings panel is closed) AND
    //   (2) The window is currently visible.
    // (1) is satisfied for the lifetime of this component. (2) we track via the
    // `window-visibility` event below: register on visible=true, clear on
    // visible=false. Initial state is computed by registerIfVisible. The binding
    // computation + platform calls live in the inputShortcuts unit.

    // Focus the textarea on show, and (de)register input shortcuts in lockstep
    // with window visibility.
    void platform()
      .windowing.subscribeVisibility((visible) => {
        if (visible) {
          focusTextarea();
          void inputShortcuts.register();
        } else {
          void inputShortcuts.clear();
        }
      })
      .then((unsubscribe) => {
        cleanups.push(unsubscribe);
      });

    // Populate the monitor list for the always-visible screen-capture select.
    void composer.loadCaptureMonitors();

    void inputShortcuts
      .subscribeEvents({
        onAttachFile: () => {
          void composer.handleAttachFile();
        },
        onCaptureScreen: () => {
          // Pick the primary monitor from the composer's snapshot, falling back
          // to the first id we see. The list is populated lazily, so fetch it
          // if it's still empty.
          void (async () => {
            if (composer.captureMonitors.length === 0) await composer.loadCaptureMonitors();
            const monitors = composer.captureMonitors;
            const target = monitors.find((m) => m.isPrimary)?.id || monitors[0]?.id;
            if (target) await composer.captureMonitorById(target);
          })();
        },
        onCaptureRegion: () => {
          void composer.handleCaptureRegionFromMenu();
        },
      })
      .then((unsubscribe) => cleanups.push(unsubscribe));
    cleanups.push(() => {
      void inputShortcuts.clear();
    });

    // Initial registration: only if the window is already visible at mount time
    // (a hidden tray-only launch should not arm shortcuts).
    void inputShortcuts.registerIfVisible();

    // Async setup: monitors, VAD, shortcut listeners, persistent-mode restore.
    // Monitors are best-effort (benign on failure); VAD and shortcut setup
    // failures surface via the placeholder notice because DevTools aren't
    // available in the packaged app.
    void (async () => {
      try {
        try {
          const m = await platform().monitors.available();
          monitors = m.map((mon, i) => ({
            id: mon.id || i.toString(),
            name: mon.name || `Monitor ${i + 1}`,
            isPrimary: mon.isPrimary,
          }));
        } catch (err) {
          log.warn("Could not fetch monitors", err);
        }

        await vadManager.attach(handleVadAudio);

        const mode = settingsState.currentSettings["stt.activation"] as ActivationMode;
        if (
          mode === "sticky" &&
          settingsState.currentSettings["stt.vadPersistedState"] === true &&
          !vadManager.enabled
        ) {
          await vadManager.toggle();
        }
      } catch (err) {
        log.error("init failed:", err);
        showInputNotice("Voice input setup failed! Check the logs.", 8000);
      }
    })();

    return () => {
      for (const c of cleanups) c();
    };
  });

  onDestroy(() => {
    vadManager.detach();
    if (inputNoticeTimeout) {
      clearTimeout(inputNoticeTimeout);
      inputNoticeTimeout = null;
    }
  });

  function handleMonitorChange(event: Event) {
    const target = event.target as HTMLSelectElement;
    const monitorId = target.value;
    settingsState.updateSetting("layout.monitor", monitorId);
  }

  function handleAlignment(align: Alignment) {
    settingsState.updateSetting("layout.alignment", align);
  }

  async function handleStop(): Promise<void> {
    await streamingState.interruptStreaming();
  }

  async function handleInterruptAndSend(): Promise<void> {
    await streamingState.interruptStreaming();
    await handleSend();
  }

  async function handleSend() {
    // Block send while any work is in flight: stream OR active tool call.
    // Otherwise the user could queue a new LLM request while tools from a
    // prior turn are still executing / awaiting input.
    if (hasActiveWork) return;
    // Block send when the core WS isn't connected. Without this the message
    // optimistically lands in the local list and the chat.start frame is
    // dropped on the floor. The user sees a queued message that never streams.
    if (connectionState.state !== "connected") return;

    const trimmedText = text.trim();
    if (!trimmedText && composer.attachments.length === 0) return;

    stt.clearDiff();

    // Expand snippet triggers before the message leaves the input. The
    // resolved user text is what gets persisted + sent to the LLM; the
    // resolved effective system prompt (including any snippet-driven
    // prepend/replace/append) is attached to the user message so sendMessages
    // can pick it up verbatim.
    const { userText, systemOverride } = applySnippets(trimmedText, snippetsState.snippets);
    const baseSystemPrompt = applySystemPromptOverride(
      buildSystemPromptBase(),
      systemOverride,
      buildContextBlock(),
    );

    const snapshot: Attachment[] = composer.attachments.map((a) => ({
      type: a.type,
      filename: a.filename,
      pendingData: a.pendingData,
      mime: a.mime,
    }));
    const timestamp = Date.now();
    const promptArgs = mcpPromptArgs;

    text = "";
    composer.clear();
    mcpPromptArgs = {};
    ac.open = false;

    try {
      // Resolve any `/prompt` MCP references into the system prompt (live
      // round-trips) before the turn starts, so the injected instructions are
      // both sent to the model and shown in the system-prompt bubble.
      const mcpBlock = await resolveMcpPromptBlock(userText, promptArgs);
      const effectiveSystemPrompt = mcpBlock
        ? baseSystemPrompt
          ? `${baseSystemPrompt}\n\n${mcpBlock}`
          : mcpBlock
        : baseSystemPrompt;
      // Persist the override whenever a snippet OR an MCP prompt shaped this
      // turn's prompt, so edit-and-resend replays the same context.
      const systemPromptOverride =
        (systemOverride || mcpBlock) && effectiveSystemPrompt ? effectiveSystemPrompt : undefined;

      await messagesState.addUserMessage({
        text: userText,
        attachments: snapshot,
        timestamp,
        systemPromptOverride,
        effectiveSystemPrompt,
      });
      await sendMessages();
    } catch (err) {
      uiLog.error("sendMessages failed:", err);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (ac.handleKey(e, autocompleteOptions, onAutocompleteSelect)) return;

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (!hasActiveWork) handleSend();
    }
  }

  async function handleVadAudio(audio: Float32Array) {
    await stt.handleVadAudio(audio, {
      getText: () => text,
      setText: (t) => {
        text = t;
      },
      focus: () => textareaElement?.focus({ preventScroll: true }),
      // Auto-send interrupts any in-flight work first, mirroring the
      // interrupt-and-send button.
      onAutoSend: async () => {
        if (streamingState.hasActiveWork) {
          await streamingState.interruptStreaming();
        }
        await handleSend();
      },
      notice: (message) => showInputNotice(message),
    });
  }

  function handleBubbleClick(e: MouseEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("button, select, textarea, a, input")) return;
    textareaElement?.focus({ preventScroll: true });
  }

  // Voice button is idle/available when the speech sidecar is up (or disabled).
  let sttIdle = $derived(sttStatus === "Running" || sttStatus === "Disabled");
  // Permission, askUser, and MCP-prompt-argument modes are driven through the
  // shared composer's `permissionPrompt` / `askUserPrompt` state; only the
  // (client-only) schedule-confirm form still rides the generic content/right
  // slots. Precedence permission > tool askUser > MCP prompt args > schedule
  // keeps one prompt in the composer at a time.
  let inAskMode = $derived(!permissionPrompt && !!pendingAsk);
  let inMcpArgMode = $derived(!permissionPrompt && !inAskMode && argMode.active);
  let inScheduleMode = $derived(
    !permissionPrompt && !inAskMode && !inMcpArgMode && !!scheduleConfirm,
  );

  // Attachments (paste, OS drop, and the attach button) are only accepted while
  // the composer is in normal compose mode. In a prompt mode (permission, tool
  // askUser, MCP prompt args, schedule-confirm) the textarea and the attach
  // controls are replaced, so a pasted or dropped file must be ignored too -
  // this mirrors exactly when the attach button is shown.
  let canAttach = $derived(!permissionPrompt && !inAskMode && !inMcpArgMode && !inScheduleMode);

  // The askUser bundle the View renders (form content + hoisted commit actions),
  // mirroring how `permissionPrompt` is derived. A tool's askUser takes the slot
  // when one is pending; otherwise the MCP prompt-argument form uses the same
  // rendering so it looks identical. Auto-submit for the no-explicit-submit
  // askUser kinds (single-select choice/files, diff, image) rides the effect below.
  let askUserPrompt = $derived.by(() => {
    if (inAskMode && ask.pending) {
      return {
        questions: ask.pending.questions,
        drafts: ask.drafts,
        togglePick: ask.togglePick,
        setText: ask.setText,
        onFreestyleFocus: ask.onFreestyleFocus,
        onFreestyleBlur: ask.onFreestyleBlur,
        setCell: ask.setCell,
        addRow: ask.addRow,
        removeRow: ask.removeRow,
        canSubmit: ask.readyToSubmit,
        onSubmit: ask.submit,
        actions: ask.actions,
      };
    }
    if (inMcpArgMode) {
      return {
        questions: argMode.questions,
        drafts: argMode.drafts,
        setText: argMode.setText,
        canSubmit: argMode.canSubmit,
        onSubmit: argMode.submit,
        actions: argMode.actions,
      };
    }
    return null;
  });
  $effect(() => {
    if (!inAskMode || ask.requiresSubmit || !ask.readyToSubmit) return;
    ask.submit();
  });
  let textareaDisabled = $derived(
    downloadsState.hasPending ||
      downloadsState.loading ||
      connectionState.coreUnavailable ||
      (llmStatus !== "Running" && llmStatus !== "Disabled"),
  );
</script>

<UserInputView
  baseColorOverride={themeOverrideHex}
  onBubbleClick={handleBubbleClick}
  bind:value={text}
  placeholder={placeholderText}
  {textareaDisabled}
  bind:textareaRef={textareaElement}
  bind:mirrorRef={mirrorSpan}
  onkeydown={handleKeydown}
  oninput={handleTextInput}
  onkeyup={handleSelectionChange}
  ontextareaclick={handleSelectionChange}
  oncompositionstart={handleCompositionStart}
  oncompositionend={handleCompositionEnd}
  ontextareablur={() => (ac.open = false)}
  onpaste={(e) => {
    if (canAttach) composer.handlePaste(e);
  }}
  topSlot={autocorrectAlert}
  contentOverride={inScheduleMode ? promptContent : undefined}
  belowContent={quickModelBar}
  attachmentSlot={attachmentRow}
  {permissionPrompt}
  onPermissionDeny={() => permissionState.respond(false)}
  onPermissionAllow={() => permissionState.respond(true)}
  {askUserPrompt}
  showLeftGroup={!inScheduleMode}
  onAttach={() => composer.handleAttachFile()}
  onAttachImage={() => composer.handleAttachImageMobile()}
  captureMonitors={composer.captureMonitors}
  onCaptureSelect={(e) => composer.handleCaptureSelect(e)}
  capturing={composer.capturing}
  onCaptureRegion={() => composer.handleCaptureRegionFromMenu()}
  showImageCapture={!!settingsState.currentSettings["llm.supportImages"]}
  {monitors}
  selectedMonitor={settingsState.getMonitor()}
  onMonitorChange={handleMonitorChange}
  onAlign={(v) => handleAlignment(v)}
  settingsTitle={downloadsState.hasPending ? "Pending downloads - open settings" : "Settings"}
  {gearTone}
  onSettings={() => viewState.navigate("settings")}
  rightSlot={inScheduleMode ? promptButtons : undefined}
  showTempToggle={sessionsState.storageEnabled && !sessionsState.started}
  tempActive={sessionsState.isTemporary}
  onTempToggle={() => sessionsState.toggleTemporary()}
  showVoice={!!settingsState.currentSettings["stt.enabled"]}
  voiceTitle={vadManager.enabled
    ? vadManager.listening
      ? "Listening..."
      : "VAD Active (click to disable)"
    : sttIdle
      ? "Enable Voice Input"
      : "Voice Input (Unavailable)"}
  voiceClass={vadManager.enabled
    ? vadManager.listening
      ? "text-accent-green-500"
      : "text-accent-blue-400"
    : sttIdle
      ? "text-default-700 hov:text-default-900"
      : "text-default-700"}
  voiceDisabled={connectionState.coreUnavailable ||
    vadManager.loading ||
    (!sttIdle && !vadManager.enabled)}
  onVoiceToggle={() => vadManager.toggle()}
  pttHolding={shortcutHandler.pttHolding}
  pttHoldDuration={shortcutHandler.pttHoldDuration}
  vadEnabled={vadManager.enabled}
  vadListening={vadManager.listening}
  {sttIdle}
  {hasActiveWork}
  {hasContent}
  sendDisabled={connectionState.coreUnavailable}
  onSend={handleSend}
  onStop={handleStop}
  onInterruptAndSend={handleInterruptAndSend}
/>

{#snippet autocorrectAlert()}
  {#if stt.showDiff && stt.original !== null}
    <AutocorrectAlert
      original={stt.original}
      onReject={() => {
        if (stt.original !== null) text = stt.original;
        stt.clearDiff();
        textareaElement?.focus({ preventScroll: true });
      }}
    />
  {/if}
{/snippet}

{#snippet promptContent()}
  {#if scheduleConfirm && prompt.scheduleDraft}
    <ScheduleConfirmForm
      draft={prompt.scheduleDraft}
      onChange={(next) => (prompt.scheduleDraft = next)}
    />
  {/if}
{/snippet}

{#snippet quickModelBar()}
  <QuickModelBar />
{/snippet}

{#snippet attachmentRow()}
  <AttachmentList
    parts={attachmentParts}
    editable
    onRemove={(i) => composer.removeAttachment(i)}
    onImageClick={(url) => composer.openImagePreview(url)}
  />
{/snippet}

{#snippet promptButtons()}
  {#if scheduleConfirm}
    <PromptButtonsView
      buttons={[
        {
          icon: "i-material-symbols-close-rounded",
          label: "Decline",
          title: "Decline this scheduled prompt",
          onClick: () => prompt.respondScheduleConfirm(false),
        },
        {
          icon: "i-material-symbols-check-rounded",
          label: "Save",
          title: "Save the scheduled prompt",
          disabled: !prompt.scheduleDraftReady,
          onClick: () => prompt.respondScheduleConfirm(true),
        },
      ]}
    />
  {/if}
{/snippet}

<svelte:window
  onblur={() => composer.closeImagePreview()}
  ondrop={(e) => {
    if (canAttach) composer.handleDrop(e);
  }}
/>

{#if ac.open}
  <div style:display="contents" style:--default-base={themeOverrideHex}>
    <SnippetAutocomplete
      options={autocompleteOptions}
      selectedIndex={ac.index}
      anchor={ac.anchor}
      onSelect={onAutocompleteSelect}
    />
  </div>
{/if}

{#if composer.previewImageUrl}
  <Modal
    open
    onclose={() => composer.closeImagePreview()}
    positioning="fixed"
    surface="transparent"
    maxWidth="fit"
    ariaLabel="Image preview"
  >
    <img
      src={composer.previewImageUrl}
      alt="Preview"
      class="max-h-[calc(100vh-6rem)] max-w-[calc(100vw-6rem)] object-contain rounded-medium shadow-2xl"
    />
  </Modal>
{/if}
