<script lang="ts">
  import type { ComponentProps } from "svelte";
  // The component gallery: every shared component rendered from its samples on a
  // dim focus grid (the homepage showcase recipe), laid out as masonry cards. NO
  // UiContext provider is mounted, so it paints from DEFAULT_UI_CONTEXT exactly
  // like a fresh desktop app. It is the visual drift/QA surface and the manual's
  // screenshot source. Sections run largest-first (whole layouts) -> stateful
  // domain Views -> objects -> overlays -> primitives -> the mobile counterparts.
  // Components that ship on top of a bubble (settings fields, the object
  // scaffolding) render in a `surface` panel so the theme flip resolves on the
  // right base; chat-message Views and modals carry their own surface. Wide
  // shells take a full-width row of their own instead of a masonry column.
  // Drift typing lives in the sample files (`satisfies`); here we cast each bundle
  // at the spread so this stays a dumb renderer that supplies only the per-View
  // child snippets. What the gallery must cover is the registry (registry.ts),
  // which the walkers parse.
  import { AGENT_ANSWER, AGENT_REASONING, SAMPLE_VALUES, SAMPLES } from "@loca/shared/ui/samples";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import AttachmentListView from "@loca/shared/ui/components/chat/AttachmentListView.svelte";
  import ChatShellView from "@loca/shared/ui/components/chat/ChatShellView.svelte";
  import { mergeFlatCorners } from "@loca/shared/ui/merge";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import ConfirmModalView from "@loca/shared/ui/components/settings/ConfirmModalView.svelte";
  import PasswordPromptModalView from "@loca/shared/ui/components/settings/PasswordPromptModalView.svelte";
  import ColorPickerModalView from "@loca/shared/ui/components/settings/ColorPickerModalView.svelte";
  import DeletionsModalView from "@loca/shared/ui/components/settings/DeletionsModalView.svelte";
  import DownloadsModalView from "@loca/shared/ui/components/settings/DownloadsModalView.svelte";
  import ShareModalView from "@loca/shared/ui/components/settings/ShareModalView.svelte";
  import PermissionRequestView from "@loca/shared/ui/components/chat/userinput/PermissionRequestView.svelte";
  import AutocorrectAlertView from "@loca/shared/ui/components/chat/userinput/AutocorrectAlertView.svelte";
  import ObjectBadgeView from "@loca/shared/ui/components/objects/ObjectBadgeView.svelte";
  import ObjectCardView from "@loca/shared/ui/components/objects/ObjectCardView.svelte";
  import ObjectDetailHeaderView from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScrollView from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import ObjectManagerView from "@loca/shared/ui/components/objects/ObjectManagerView.svelte";
  import CoresFieldView from "@loca/shared/ui/components/settings/CoresFieldView.svelte";
  import ServicesFieldView from "@loca/shared/ui/components/settings/ServicesFieldView.svelte";
  import StorageFieldView from "@loca/shared/ui/components/settings/StorageFieldView.svelte";
  import ExtensionDetailView from "@loca/shared/ui/components/settings/ExtensionDetailView.svelte";
  import ExtensionsFieldView from "@loca/shared/ui/components/settings/ExtensionsFieldView.svelte";
  import ToolDetailView from "@loca/shared/ui/components/settings/ToolDetailView.svelte";
  import ToolsFieldView from "@loca/shared/ui/components/settings/ToolsFieldView.svelte";
  import McpDetailView from "@loca/shared/ui/components/settings/McpDetailView.svelte";
  import McpFieldView from "@loca/shared/ui/components/settings/McpFieldView.svelte";
  import MemoryDetailView from "@loca/shared/ui/components/settings/MemoryDetailView.svelte";
  import ModelPresetFieldView from "@loca/shared/ui/components/settings/ModelPresetFieldView.svelte";
  import SttPresetFieldView from "@loca/shared/ui/components/settings/SttPresetFieldView.svelte";
  import TtsPresetFieldView from "@loca/shared/ui/components/settings/TtsPresetFieldView.svelte";
  import ShortcutFieldView from "@loca/shared/ui/components/settings/ShortcutFieldView.svelte";
  import ScheduleEditorView from "@loca/shared/ui/components/chat/ScheduleEditorView.svelte";
  import PromptButtonsView from "@loca/shared/ui/components/chat/userinput/PromptButtonsView.svelte";
  import NewCoreWizardView from "@loca/shared/ui/components/new-core/NewCoreWizardView.svelte";
  import ErrorMessageView from "@loca/shared/ui/components/chat/messages/ErrorMessageView.svelte";
  import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";
  import RelevantMemoriesView from "@loca/shared/ui/components/chat/messages/RelevantMemoriesView.svelte";
  import RelevantToolsView from "@loca/shared/ui/components/chat/messages/RelevantToolsView.svelte";
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import CoreBarView from "@loca/shared/ui/components/chat/CoreBarView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import SettingsFieldView from "@loca/shared/ui/components/settings/SettingsFieldView.svelte";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SnippetAutocompleteView from "@loca/shared/ui/components/chat/SnippetAutocompleteView.svelte";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import CommandPreviewFieldView from "@loca/shared/ui/components/settings/CommandPreviewFieldView.svelte";
  import MessageStackView from "@loca/shared/ui/components/chat/MessageStackView.svelte";
  import SessionListView from "@loca/shared/ui/components/session-list/SessionListView.svelte";
  import UpdateButtonView from "@loca/shared/ui/components/settings/UpdateButtonView.svelte";
  import QuickSettingsView from "@loca/shared/ui/components/quick-settings/QuickSettingsView.svelte";
  import ScheduleConfirmFormView from "@loca/shared/ui/components/chat/userinput/ScheduleConfirmFormView.svelte";
  import SnippetDetailView from "@loca/shared/ui/components/settings/SnippetDetailView.svelte";
  import ScheduledPromptDetailView from "@loca/shared/ui/components/settings/ScheduledPromptDetailView.svelte";
  import { SETTINGS_SCHEMA } from "@loca/shared/domain/settings/engine";
  import SettingsDemoFooter from "../demos/SettingsDemoFooter.svelte";
  import GalleryCard from "./GalleryCard.svelte";
  import Primitives from "./Primitives.svelte";
  import MobileGallery from "./MobileGallery.svelte";
  import Bubble from "@loca/shared/ui/components/primitives/Bubble.svelte";

  const entries = <T,>(o: Record<string, T>) => Object.entries(o);
  const chatNoop = (): void => {};

  // Representative card metadata for the ObjectManagerView sample items, keyed by
  // name, so the gallery's list rows render as the full ObjectCardView (badges,
  // description, author/meta, triple-dot) the client shows, not a bare label.
  type ObjectBadge = NonNullable<ComponentProps<typeof ObjectCardView>["badges"]>[number];
  const OBJECT_CARD_META: Record<
    string,
    { description: string; meta: string; badges: ObjectBadge[] }
  > = {
    "Code Search": {
      description: "Searches the codebase for symbols, references, and text matches.",
      meta: "Built-in extension",
      badges: [
        { label: "Enabled", accent: "green" },
        { label: "Updated", accent: "blue" },
      ],
    },
    filesystem: {
      description: "Local (stdio)",
      meta: "node ./server.js",
      badges: [{ label: "Error", accent: "red", title: "Failed to start" }],
    },
    "Daily summary": {
      description: "Runs every weekday at 9:00 AM.",
      meta: "Scheduled prompt",
      badges: [{ label: "Enabled", accent: "green" }],
    },
  };

  const galleryUi = useUiContext();

  // A short simulated session for the ChatShellView card, so the shell shows the
  // real chat the way the client paints it (mirrors the homepage ChatStage).
  const CHAT_PROMPT = "How do I install Loca on macOS?";
  const CHAT_REASONING =
    "The user is on macOS, so the one-line installer is the simplest path; point them at the landing-page command and the launch step.";
  const CHAT_ANSWER =
    "Run the one-line installer from the landing page, then launch Loca from Applications. Want the per-OS steps?";

  // The color picker is a Popover anchored to an element; each card mounts its
  // own stand-in anchor so the popover positions inside that card's frame. Keyed
  // by sample name (a single shared ref would be overwritten by the last card in
  // the loop, so every popover would anchor to the last button).
  let colorAnchors = $state<Record<string, HTMLElement | null>>({});

  // Static render: SettingsContentView's expand-on-mount `$effect` never runs, so
  // pass the default-open section keys explicitly (one per labeled, not
  // defaultCollapsed section) to match the live app.
  function expandedFor(gid: string | undefined): Set<string> {
    const keys = new Set<string>();
    if (gid) {
      SETTINGS_SCHEMA.find((g) => g.id === gid)?.sections.forEach((s, i) => {
        if (s.label && !s.defaultCollapsed) keys.add(`${gid}-${i}`);
      });
    }
    return keys;
  }
</script>

<div class="mx-auto max-w-6xl px-4 py-10 flex flex-col gap-12">
  <header class="flex flex-col gap-2">
    <h1 class="text-2xl font-semibold">Component Gallery</h1>
    <p class="text-default-500 text-sm">
      A one to one replica of Loca Client components in various states.
    </p>
    <p class="text-default-500 text-sm">
      The Client is built with Tauri and Svelte, and both it and this page shares one UI package. A
      state shown here should be identical to the one in the Client.
    </p>
    <p class="text-default-500 text-sm">
      If anything looks off, please <a
        class="underline"
        href="https://github.com/rzkyif/loca/issues">report it as an issue.</a
      >
    </p>
  </header>

  <!-- Largest first: whole layout shells, each on a full-width row of its own. -->
  <section class="flex flex-col gap-6">
    <h2 class="text-lg font-medium">Layouts</h2>
    <div class="flex flex-col gap-4">
      {#each entries(SAMPLES.ChatShellView) as [name, p] (name)}
        <GalleryCard label={`ChatShellView · ${name} · desktop`} wide>
          <!-- No fixed height + overflow-hidden: the desktop shell is a
               flex-col-reverse column anchored at the bottom, so a capped height
               clips the TOP (oldest) row. Let it size to its content instead so
               the whole simulated session is visible. -->
          <div class="w-full flex justify-center">
            <ChatShellView
              {...p}
              coreBar={chatCoreBar}
              sessionBar={chatSessionBar}
              input={chatInput}
              transcript={chatTranscript}
            />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SettingsShellView) as [name, p] (name)}
        <GalleryCard label={`SettingsShellView · ${name}`} wide>
          <!-- The settings view pins the CoreBar flush against the panel's bottom:
               the panel squares its bottom corners onto the merged CoreBar (the
               narrower plug, top corners squared + pulled up to overlap). -->
          <div class="flex flex-col" style:gap={bubbleGap(galleryUi)}>
            <SettingsShellView
              {...p}
              sizeClass="w-full h-[28rem]"
              flatCorners={mergeFlatCorners("center", "bottom", false)}
            >
              {#snippet groupContent(gid)}
                <SettingsContentView
                  groupId={gid}
                  values={SAMPLE_VALUES}
                  expanded={expandedFor(gid)}
                />
              {/snippet}
              {#snippet sidebarFooter(collapsed)}
                <SettingsDemoFooter {collapsed} />
              {/snippet}
            </SettingsShellView>
            <CoreBarView
              {...SAMPLES.CoreBarView.idle}
              merge={{ flatCorners: mergeFlatCorners("center", "top", true), overlapTop: true }}
            />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.NewCoreWizardView) as [name, p] (name)}
        <GalleryCard label={`NewCoreWizardView · ${name}`} wide>
          <div class="w-full flex justify-center">
            <NewCoreWizardView {...p} />
          </div>
        </GalleryCard>
      {/each}
    </div>
  </section>

  <!-- Stateful chat Views: messages, bars, composer. Each renders its own bubble
       or chat-area chrome, so they sit straight on the grid (no surface panel). -->
  <section class="flex flex-col gap-6">
    <h2 class="text-lg font-medium">Chat</h2>

    <!-- The composer, the two bars, the session list, and the message stack are
         the full-window-width chat pieces; a narrow masonry tile would clip them
         (and their drop shadows), so each takes a centered full-width row where it
         renders at the faithful 700px-window proportions. -->
    <div class="flex flex-col gap-4">
      {#each entries(SAMPLES.UserInputView) as [name, p] (name)}
        <GalleryCard label={`UserInputView · ${name}`} wide>
          <div class="w-full flex justify-center">
            <UserInputView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SessionBarView) as [name, p] (name)}
        <GalleryCard label={`SessionBarView · ${name}`} wide>
          <div class="w-full flex justify-center">
            <div class="w-[40rem] max-w-full">
              <SessionBarView {...p} />
            </div>
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SessionListView) as [name, p] (name)}
        <GalleryCard label={`SessionListView · ${name}`} wide>
          <div class="w-full flex justify-center">
            <SessionListView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.MessageStackView) as [name, p] (name)}
        {@const sp = p}
        <GalleryCard label={`MessageStackView · ${name}`} wide>
          <!-- Force the host narrower than its content for the multi-bubble samples
               so the row GENUINELY overflows and clips (a static page cannot script
               a live scroll, so without this the fade would hint at hidden content
               that actually fits). Center the overflow for the "scrolled into the
               middle" sample so BOTH ends clip and both fades are truthful. See
               site.css `.stack-demo-*`. -->
          <div
            class={`w-full flex justify-center ${sp.count > 1 ? "stack-demo-narrow" : ""} ${
              name === "rightScrolled" ? "stack-demo-center" : ""
            }`}
          >
            <MessageStackView {...sp}>
              {#snippet bubble(i)}
                <!-- A stack row is the client's row of COLLAPSED expandable message
                     bubbles, so render that exact shared component (a `size="small"`
                     Bubble wrapping a chevron header), collapsed by default. A plain
                     pill was both the wrong shape and taller than the real header
                     strip. Adjacent corners flatten via neighbor flags like the app. -->
                <ExpandableMessageView
                  title={`Tool call ${i + 1}`}
                  text="Collapsed detail body."
                  neighborLeft={i > 0}
                  neighborRight={i < sp.count - 1}
                />
              {/snippet}
            </MessageStackView>
          </div>
        </GalleryCard>
      {/each}
    </div>

    <div class="columns-1 sm:columns-2 lg:columns-3 gap-4">
      {#each entries(SAMPLES.UserMessageView) as [name, p] (name)}
        <GalleryCard label={`UserMessageView · ${name}`}>
          <UserMessageView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.AgentMessageView) as [name, p] (name)}
        <GalleryCard label={`AgentMessageView · ${name}`}>
          <AgentMessageView {...p}>
            {#snippet body()}
              <span class="whitespace-pre-wrap break-words"
                >{p.kind === "content" ? AGENT_ANSWER : AGENT_REASONING}</span
              >
            {/snippet}
          </AgentMessageView>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ToolCallView) as [name, p] (name)}
        <GalleryCard label={`ToolCallView · ${name}`}>
          <ToolCallView {...p}>
            {#snippet memoryContent({ content })}
              <pre class="whitespace-pre-wrap text-xs">{content}</pre>
            {/snippet}
          </ToolCallView>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ErrorMessageView) as [name, p] (name)}
        <GalleryCard label={`ErrorMessageView · ${name}`}>
          <ErrorMessageView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ExpandableMessageView) as [name, p] (name)}
        <GalleryCard label={`ExpandableMessageView · ${name}`}>
          <ExpandableMessageView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.RelevantMemoriesView) as [name, p] (name)}
        <GalleryCard label={`RelevantMemoriesView · ${name}`}>
          <RelevantMemoriesView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.RelevantToolsView) as [name, p] (name)}
        <GalleryCard label={`RelevantToolsView · ${name}`}>
          <RelevantToolsView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.AttachmentListView) as [name, p] (name)}
        <GalleryCard label={`AttachmentListView · ${name}`} surface hug>
          <AttachmentListView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.CoreBarView) as [name, p] (name)}
        <GalleryCard label={`CoreBarView · ${name}`}>
          <CoreBarView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.PermissionRequestView) as [name, p] (name)}
        <GalleryCard label={`PermissionRequestView · ${name}`} surface hug>
          <PermissionRequestView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.AutocorrectAlertView) as [name, p] (name)}
        <GalleryCard label={`AutocorrectAlertView · ${name}`}>
          <!-- The client renders this alert inside the composer bubble (it arrives
               as the composer's top slot), so the gallery wraps it in the same
               shared Bubble rather than showing it bare on the grid. -->
          <Bubble selectedAlignment="left" extraClass="w-full">
            <AutocorrectAlertView {...p} />
          </Bubble>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.PromptButtonsView) as [name, p] (name)}
        <GalleryCard label={`PromptButtonsView · ${name}`} surface hug>
          <PromptButtonsView {...p} />
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SnippetAutocompleteView) as [name, p] (name)}
        <GalleryCard label={`SnippetAutocompleteView · ${name}`} backdrop>
          <!-- The dropdown is position:fixed; this inner transformed stage is its
               containing block, so a centered anchor lands it in the middle of a
               known-size box instead of the sample's top-left {0,0}. -->
          <div
            class="relative mx-auto w-[20rem] h-48 overflow-hidden"
            style="transform: translateZ(0)"
          >
            <SnippetAutocompleteView {...p} anchor={{ top: 24, left: 32 }} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ScheduleConfirmFormView) as [name, p] (name)}
        <GalleryCard label={`ScheduleConfirmFormView · ${name}`} surface>
          <div class="w-full">
            <ScheduleConfirmFormView {...p} />
          </div>
        </GalleryCard>
      {/each}
    </div>
  </section>

  <!-- Settings field-level Views, each on the bg-surface panel they ship inside. -->
  <section class="flex flex-col gap-6">
    <h2 class="text-lg font-medium">Settings</h2>
    <div class="columns-1 sm:columns-2 gap-4">
      {#each entries(SAMPLES.SettingsFieldView) as [name, p] (name)}
        <GalleryCard label={`SettingsFieldView · ${name}`} surface>
          <div class="w-full">
            <SettingsFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.CoresFieldView) as [name, p] (name)}
        <GalleryCard label={`CoresFieldView · ${name}`} surface>
          <!-- Match the gap the client's ObjectDetailScrollView puts between the
               field sections, so the gallery shows the same section spacing. -->
          <div class="w-full flex flex-col gap-3">
            <CoresFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ServicesFieldView) as [name, p] (name)}
        <GalleryCard label={`ServicesFieldView · ${name}`} surface>
          <div class="w-full">
            <ServicesFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.StorageFieldView) as [name, p] (name)}
        <GalleryCard label={`StorageFieldView · ${name}`} surface>
          <div class="w-full">
            <StorageFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ExtensionsFieldView) as [name, p] (name)}
        <GalleryCard label={`ExtensionsFieldView · ${name}`} surface>
          <div class="w-full">
            <ExtensionsFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ExtensionDetailView) as [name, p] (name)}
        <GalleryCard label={`ExtensionDetailView · ${name}`} surface>
          <div class="w-full">
            <ExtensionDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ToolsFieldView) as [name, p] (name)}
        <GalleryCard label={`ToolsFieldView · ${name}`} surface>
          <div class="w-full">
            <ToolsFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ToolDetailView) as [name, p] (name)}
        <GalleryCard label={`ToolDetailView · ${name}`} surface>
          <div class="w-full">
            <ToolDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.McpDetailView) as [name, p] (name)}
        <GalleryCard label={`McpDetailView · ${name}`} surface>
          <div class="w-full">
            <McpDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.McpFieldView) as [name, p] (name)}
        <GalleryCard label={`McpFieldView · ${name}`} surface>
          <div class="w-full">
            <McpFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.MemoryDetailView) as [name, p] (name)}
        <GalleryCard label={`MemoryDetailView · ${name}`} surface>
          <div class="w-full">
            <MemoryDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ModelPresetFieldView) as [name, p] (name)}
        <GalleryCard label={`ModelPresetFieldView · ${name}`} surface>
          <div class="w-full">
            <ModelPresetFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SttPresetFieldView) as [name, p] (name)}
        <GalleryCard label={`SttPresetFieldView · ${name}`} surface>
          <div class="w-full">
            <SttPresetFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.TtsPresetFieldView) as [name, p] (name)}
        <GalleryCard label={`TtsPresetFieldView · ${name}`} surface>
          <div class="w-full">
            <TtsPresetFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ShortcutFieldView) as [name, p] (name)}
        <GalleryCard label={`ShortcutFieldView · ${name}`} surface>
          <div class="w-full">
            <ShortcutFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ScheduleEditorView) as [name, p] (name)}
        <GalleryCard label={`ScheduleEditorView · ${name}`} surface>
          <div class="w-full">
            <ScheduleEditorView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ScheduledPromptDetailView) as [name, p] (name)}
        <GalleryCard label={`ScheduledPromptDetailView · ${name}`} surface>
          <div class="w-full">
            <ScheduledPromptDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.SnippetDetailView) as [name, p] (name)}
        <GalleryCard label={`SnippetDetailView · ${name}`} surface>
          <div class="w-full">
            <SnippetDetailView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.CommandPreviewFieldView) as [name, p] (name)}
        <GalleryCard label={`CommandPreviewFieldView · ${name}`} surface>
          <div class="w-full min-w-0">
            <CommandPreviewFieldView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.UpdateButtonView) as [name, p] (name)}
        <GalleryCard label={`UpdateButtonView · ${name}`} surface hug>
          <div class="w-full">
            <UpdateButtonView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.QuickSettingsView) as [name, p] (name)}
        <GalleryCard label={`QuickSettingsView · ${name}`} surface>
          <!-- Render at the faithful desktop-panel width so the gallery matches
               the client; the View itself owns its layout and the real schema
               fields, so no snippet stand-ins are needed. -->
          <div class="w-[34rem] max-w-full">
            <QuickSettingsView {...p} />
          </div>
        </GalleryCard>
      {/each}
    </div>
  </section>

  <!-- Object master/detail scaffolding shared across the settings field lists. -->
  <section class="flex flex-col gap-6">
    <h2 class="text-lg font-medium">Objects</h2>
    <div class="columns-1 sm:columns-2 gap-4">
      {#each entries(SAMPLES.ObjectManagerView) as [name, p] (name)}
        <GalleryCard label={`ObjectManagerView · ${name}`} surface>
          <!-- A plain block (not a row flex) so the View, which is `display:flex`,
               fills the panel width and its toolbar (search + triple-dot) stretches
               like the client, instead of shrinking to content. -->
          <div class="w-full h-80">
            <ObjectManagerView {...p}>
              {#snippet card(item: { name: string }, open: () => void)}
                <ObjectCardView
                  label={item.name}
                  description={OBJECT_CARD_META[item.name]?.description}
                  meta={OBJECT_CARD_META[item.name]?.meta}
                  badges={OBJECT_CARD_META[item.name]?.badges ?? []}
                  hasMenu
                  onOpen={open}
                  onMenu={chatNoop}
                />
              {/snippet}
              {#snippet detailPane(item: { name: string })}
                <ObjectDetailHeaderView title={item.name} />
              {/snippet}
              {#snippet empty()}
                <div class="text-default-500 text-sm p-4">No items yet.</div>
              {/snippet}
            </ObjectManagerView>
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ObjectCardView) as [name, p] (name)}
        <GalleryCard label={`ObjectCardView · ${name}`} surface>
          <div class="w-full">
            <ObjectCardView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ObjectDetailHeaderView) as [name, p] (name)}
        <GalleryCard label={`ObjectDetailHeaderView · ${name}`} surface>
          <div class="w-full">
            <ObjectDetailHeaderView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ObjectDetailScrollView) as [name, p] (name)}
        <GalleryCard label={`ObjectDetailScrollView · ${name}`} surface>
          <div class="w-full">
            <ObjectDetailScrollView {...p}>
              <div class="text-default-600 text-sm">Type-specific detail content.</div>
            </ObjectDetailScrollView>
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ObjectBadgeView) as [name, p] (name)}
        <GalleryCard label={`ObjectBadgeView · ${name}`} surface hug>
          <ObjectBadgeView {...p} />
        </GalleryCard>
      {/each}
    </div>
  </section>

  <!-- Conditional overlays: modals and popovers, each open over a dimmed backdrop
       pinned to the card, with the focus grid showing behind the scrim. -->
  <section class="flex flex-col gap-6">
    <h2 class="text-lg font-medium">Overlays</h2>
    <div class="columns-1 sm:columns-2 gap-4">
      {#each entries(SAMPLES.ConfirmModalView) as [name, p] (name)}
        <GalleryCard label={`ConfirmModalView · ${name}`} backdrop>
          <div class="relative h-80 w-full">
            <ConfirmModalView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.PasswordPromptModalView) as [name, p] (name)}
        <!-- Tall enough that the error state (the extra alert row) clears the
             gallery's `calc(100% - 4rem)` dialog cap, so the modal never scrolls. -->
        <GalleryCard label={`PasswordPromptModalView · ${name}`} backdrop>
          <div class="relative h-[22rem] w-full">
            <PasswordPromptModalView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.DeletionsModalView) as [name, p] (name)}
        <GalleryCard label={`DeletionsModalView · ${name}`} backdrop>
          <div class="relative h-[30rem] w-full">
            <DeletionsModalView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.DownloadsModalView) as [name, p] (name)}
        <GalleryCard label={`DownloadsModalView · ${name}`} backdrop>
          <div class="relative h-[28rem] w-full">
            <DownloadsModalView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ShareModalView) as [name, p] (name)}
        <!-- The dialog is h-[70vh] capped at max-h-[40rem]; size the card taller
             than that cap so the modal is never clipped AND keeps clear vertical
             breathing room above and below it on the grid. -->
        <GalleryCard label={`ShareModalView · ${name}`} backdrop>
          <div class="relative h-[44rem] w-full">
            <ShareModalView {...p} />
          </div>
        </GalleryCard>
      {/each}

      {#each entries(SAMPLES.ColorPickerModalView) as [name, p] (name)}
        <GalleryCard label={`ColorPickerModalView · ${name}`} backdrop>
          <div class="relative h-[26rem] w-full flex items-start justify-center pt-4">
            <button
              bind:this={colorAnchors[name]}
              class="rounded-medium bg-surface-inset px-3 py-1.5 text-sm text-default-700"
            >
              Pick color
            </button>
            <ColorPickerModalView {...p} anchor={colorAnchors[name] ?? null} />
          </div>
        </GalleryCard>
      {/each}
    </div>
  </section>

  <Primitives />

  <!-- Mobile counterparts, under a mobile UiContext (scoped to its own subtree
       so the desktop cards above stay on DEFAULT_UI_CONTEXT). -->
  <MobileGallery />
</div>

<!-- Chat-shell regions for the desktop ChatShellView card: the REAL shared
     components on a short simulated session, fed the same samples the client
     feeds live state, so the card shows the actual chat (core bar, session bar,
     composer, and a prompt -> reasoning -> answer exchange), not stand-ins. -->
{#snippet chatCoreBar()}
  <!-- Desktop: the CoreBar is the lower, narrower bubble, so it merges UP into the
       SessionBar (both top corners squared, pulled up to overlap the seam). -->
  <CoreBarView
    {...SAMPLES.CoreBarView.idle}
    merge={{ flatCorners: mergeFlatCorners("center", "top", true), overlapTop: true }}
  />
{/snippet}
{#snippet chatSessionBar(_z: number)}
  <!-- The session-management buttons (list + new) always show with the bar, so
       the shell card matches the client instead of a bar with no controls. The
       SessionBar is the wider bubble and keeps its rounding (center-aligned). -->
  <SessionBarView
    {...SAMPLES.SessionBarView.default}
    onList={chatNoop}
    onNew={chatNoop}
    merge={{ flatCorners: mergeFlatCorners("center", "bottom", false), overlapTop: false }}
  />
{/snippet}
{#snippet chatInput()}
  <UserInputView {...SAMPLES.UserInputView.empty} />
{/snippet}
{#snippet chatTranscript()}
  <!-- Newest-first DOM order: the shell's transcript column is flex-col-reverse,
       so the answer (first here) lands at the visual bottom by the composer and
       the prompt (last) at the top, reading top-to-bottom chronologically. -->
  <AgentMessageView kind="content" bgClass="bubble-agent">
    {#snippet body()}
      <span class="whitespace-pre-wrap break-words">{CHAT_ANSWER}</span>
    {/snippet}
  </AgentMessageView>
  <AgentMessageView kind="reasoning" reasoningDurationMs={3200}>
    {#snippet body()}
      <span>{CHAT_REASONING}</span>
    {/snippet}
  </AgentMessageView>
  <UserMessageView text={CHAT_PROMPT} />
{/snippet}
