<script lang="ts">
  // Presentational body of a memory's detail pane: the enable toggle with a
  // kind/read-only subtitle, the summarization status, the title field, and the
  // body editor with an edit/preview toggle. A knowledge memory has one body
  // ("Content"); a skill splits into three synced fields ("Description",
  // "Suggested tools", "Instructions") that the client recomposes into the one
  // SKILL.md, plus the bundled-files list with an inline file viewer/editor. All
  // values arrive pre-resolved (the client owns the store, the loaded content,
  // the editability and skill flags, the status, and the draft wiring), so this
  // stays pure: props in, callbacks out. The detail header and scroll shell live
  // in ../objects/*; this is only MemoryDetail's own body markup. `draftTitle`
  // is plain; `draftDescription`, `draftContent`, and `openFileContent` are
  // $bindable to mirror the client's input/textarea drafts.
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Textarea from "../primitives/Textarea.svelte";
  import Toggle from "../primitives/Toggle.svelte";
  import Button from "../primitives/Button.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import Card from "../primitives/Card.svelte";
  import Expand from "../primitives/Expand.svelte";
  import Chip from "../primitives/Chip.svelte";
  import ListItem from "../primitives/ListItem.svelte";
  import IconText from "../primitives/IconText.svelte";

  let {
    enabled = false,
    isSkill = false,
    editable = false,
    draftTitle = "",
    titleError = null,
    contentLoaded = false,
    summaryStale = false,
    summary = "",
    draftDescription = $bindable(""),
    suggestedTools = [],
    availableTools = [],
    files = [],
    draftContent = $bindable(""),
    openFileName = null,
    openFileContent = $bindable(""),
    openFileLoaded = false,
    onToggleEnabled,
    onTitleInput,
    onTitleBlur,
    onDescriptionInput,
    onDescriptionBlur,
    onSuggestedToolsChange,
    onContentInput,
    onContentBlur,
    onOpenFile,
    onCloseFile,
    onFileContentInput,
    onFileContentBlur,
    onAddFile,
    onDeleteFile,
  }: {
    enabled?: boolean;
    isSkill?: boolean;
    editable?: boolean;
    draftTitle?: string;
    titleError?: string | null;
    contentLoaded?: boolean;
    summaryStale?: boolean;
    summary?: string;
    draftDescription?: string;
    suggestedTools?: string[];
    availableTools?: string[];
    files?: string[];
    draftContent?: string;
    openFileName?: string | null;
    openFileContent?: string;
    openFileLoaded?: boolean;
    onToggleEnabled?: (enabled: boolean) => void;
    onTitleInput?: (v: string) => void;
    onTitleBlur?: () => void;
    onDescriptionInput?: (v: string) => void;
    onDescriptionBlur?: () => void;
    onSuggestedToolsChange?: (tools: string[]) => void;
    onContentInput?: (v: string) => void;
    onContentBlur?: () => void;
    onOpenFile?: (name: string) => void;
    onCloseFile?: () => void;
    onFileContentInput?: (v: string) => void;
    onFileContentBlur?: () => void;
    onAddFile?: (name: string) => void;
    onDeleteFile?: (name: string) => void;
  } = $props();

  const noop = (): void => {};

  // Inline "new bundled file" name entry, view-local: the + button reveals it,
  // submitting hands the trimmed name to the client (which creates + opens it).
  let addingFile = $state(false);
  let newFileName = $state("");
  function submitNewFile(): void {
    const name = newFileName.trim();
    if (name) (onAddFile ?? noop)(name);
    newFileName = "";
    addingFile = false;
  }

  // Suggested-tools chip multi-select (skills only). View-local typing state;
  // selections flow out through onSuggestedToolsChange. `available` is the tool
  // catalog the client feeds (the website feeds a scripted one).
  let toolQuery = $state("");
  let toolFocused = $state(false);
  let toolActiveIndex = $state(0);
  let toolInputEl: HTMLInputElement | undefined = $state();
  const toolSuggestions = $derived.by(() => {
    const q = toolQuery.trim().toLowerCase();
    return availableTools
      .filter((t) => !suggestedTools.includes(t))
      .filter((t) => !q || t.toLowerCase().includes(q))
      .slice(0, 8);
  });
  const toolsOpen = $derived(toolFocused && toolSuggestions.length > 0);
  function addTool(tool: string): void {
    const t = tool.trim();
    toolQuery = "";
    toolActiveIndex = 0;
    if (!t || suggestedTools.includes(t)) return;
    (onSuggestedToolsChange ?? noop)([...suggestedTools, t]);
  }
  function removeTool(tool: string): void {
    (onSuggestedToolsChange ?? noop)(suggestedTools.filter((t) => t !== tool));
  }
  // Commit the highlighted suggestion, or the typed text, as a chip.
  function commitTool(): void {
    if (toolsOpen && toolSuggestions[toolActiveIndex]) addTool(toolSuggestions[toolActiveIndex]);
    else if (toolQuery.trim()) addTool(toolQuery);
  }
  function onToolKeydown(e: KeyboardEvent): void {
    // Tool names allow only [a-zA-Z0-9_-], so any other printable key (notably
    // space and comma) terminates the current chip instead of being typed,
    // just like Enter. Modifier combos (Cmd/Ctrl/Alt) are left to the browser.
    const illegalChar =
      e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && !/^[a-zA-Z0-9_-]$/.test(e.key);
    if (e.key === "Enter" || illegalChar) {
      e.preventDefault();
      commitTool();
    } else if (e.key === "Backspace" && toolQuery === "" && suggestedTools.length > 0) {
      removeTool(suggestedTools[suggestedTools.length - 1]);
    } else if (e.key === "ArrowDown" && toolsOpen) {
      e.preventDefault();
      toolActiveIndex = (toolActiveIndex + 1) % toolSuggestions.length;
    } else if (e.key === "ArrowUp" && toolsOpen) {
      e.preventDefault();
      toolActiveIndex = (toolActiveIndex - 1 + toolSuggestions.length) % toolSuggestions.length;
    } else if (e.key === "Escape") {
      toolQuery = "";
    }
  }

  // Summarization status: a read-only two-line display. The summary regenerates
  // on its own in the background a short while after an edit, so there is no
  // action here. While the summary is stale (never generated, or edited since)
  // the memory won't surface by relevance, which is what the line explains; once
  // current, the second line shows the generated summary itself.
  const status = $derived(
    summaryStale
      ? {
          icon: "i-material-symbols-info-rounded",
          tone: "text-accent-yellow-700",
          title: "Pending summarization",
          detail: "Won't surface by relevance until summarization finishes.",
        }
      : {
          icon: "i-material-symbols-check-circle-rounded",
          tone: "text-default-500",
          title: "Summarized",
          detail: summary.trim() || "Can surface when it's relevant to your message.",
        },
  );
</script>

<div class="flex flex-col gap-3">
  <FormField
    label="Enabled"
    description={`${isSkill ? "Skill" : "Knowledge"}${
      editable ? "" : " · provided by an extension (read-only)"
    }`}
    descriptionTier="always"
    horizontal
  >
    <Toggle
      checked={enabled}
      ariaLabel="Enable memory"
      onchange={(v) => (onToggleEnabled ?? noop)(v)}
    />
  </FormField>

  <div class="flex flex-col gap-0.5 text-xs">
    <IconText icon={status.icon} color={status.tone}>{status.title}</IconText>
    <span class="text-default-500">{status.detail}</span>
  </div>

  <FormField label="Title" error={editable ? titleError : null}>
    <Input
      type="text"
      value={draftTitle}
      ariaLabel="Memory title"
      disabled={!editable}
      error={editable ? !!titleError : false}
      oninput={(v) => (onTitleInput ?? noop)(v)}
      onblur={() => (onTitleBlur ?? noop)()}
    />
  </FormField>

  {#if isSkill}
    <FormField label="Description">
      <Input
        type="text"
        value={draftDescription}
        ariaLabel="Skill description"
        placeholder="What this skill produces"
        disabled={!editable}
        oninput={(v) => (onDescriptionInput ?? noop)(v)}
        onblur={() => (onDescriptionBlur ?? noop)()}
      />
    </FormField>

    <FormField label="Suggested Tools">
      <div class="relative">
        <!-- The whole field focuses the input, so the chips + empty space read
             as one control; the inner <input> is the real focus target. -->
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <div
          class="loca-focus-wrap flex flex-wrap items-center gap-1.5 w-full min-h-8 rounded-medium bg-surface-inset px-2 py-1 {editable
            ? 'hov:cursor-text'
            : 'opacity-60 pointer-events-none'}"
          onclick={() => toolInputEl?.focus()}
        >
          {#each suggestedTools as tool (tool)}
            <Chip size="sm" variant="subtle">
              <span class="font-mono truncate">{tool}</span>
              {#if editable}
                <button
                  type="button"
                  class="flex text-default-500 hov:text-default-800 transition-interactive"
                  aria-label={`Remove ${tool}`}
                  onclick={(e) => {
                    e.stopPropagation();
                    removeTool(tool);
                  }}
                >
                  <i class="i-material-symbols-close-rounded text-sm flex"></i>
                </button>
              {/if}
            </Chip>
          {/each}
          {#if editable}
            <input
              bind:this={toolInputEl}
              bind:value={toolQuery}
              class="flex-1 min-w-24 bg-transparent outline-none text-sm font-mono text-default-800"
              aria-label="Add a suggested tool"
              placeholder={suggestedTools.length > 0 ? "" : "Add a tool"}
              onfocus={() => (toolFocused = true)}
              onblur={() => (toolFocused = false)}
              onkeydown={onToolKeydown}
            />
          {/if}
        </div>
        {#if toolsOpen}
          <div
            class="loca-scroll absolute z-50 left-0 right-0 mt-1 bg-surface rounded-medium shadow-xl border border-surface overflow-hidden max-h-60 overflow-y-auto"
            role="listbox"
          >
            {#each toolSuggestions as tool, i (tool)}
              <!-- onmousedown + preventDefault keeps the input focused through
                   the pick (so the dropdown doesn't blur-close first). -->
              <ListItem
                direction="row"
                selected={i === toolActiveIndex}
                role="option"
                ariaSelected={i === toolActiveIndex}
                class="rounded-none px-3"
                onmousedown={(e) => {
                  e.preventDefault();
                  addTool(tool);
                }}
              >
                <span class="truncate flex-1 text-sm font-mono text-default-800">{tool}</span>
              </ListItem>
            {/each}
          </div>
        {/if}
      </div>
    </FormField>
  {/if}

  <FormField label={isSkill ? "Instructions" : "Content"}>
    <Textarea
      ariaLabel="Memory content"
      autoResize="none"
      mono
      class="min-h-48 overflow-y-auto resize-y"
      value={draftContent}
      placeholder={contentLoaded ? "" : "Loading..."}
      disabled={!contentLoaded || !editable}
      oninput={(v) => (onContentInput ?? noop)(v)}
      onblur={() => (onContentBlur ?? noop)()}
    />
  </FormField>

  {#if isSkill}
    <div class="flex flex-col gap-1">
      <div class="flex items-center gap-2 min-h-8">
        <div class="flex-1 text-default-800 text-sm">Bundled Files</div>
        {#if editable}
          <IconButton
            icon="i-material-symbols-add-rounded"
            title="Add file"
            size="xs"
            variant="subtle"
            active={addingFile}
            onclick={() => (addingFile = !addingFile)}
          />
        {/if}
      </div>
      {#if addingFile}
        <div class="flex items-center gap-2">
          <Input
            type="text"
            value={newFileName}
            ariaLabel="New file name"
            placeholder="checklist.md"
            mono
            class="flex-1 text-xs"
            oninput={(v) => (newFileName = v)}
            onkeydown={(e) => {
              if (e.key === "Enter") submitNewFile();
              else if (e.key === "Escape") {
                newFileName = "";
                addingFile = false;
              }
            }}
          />
          <Button variant="secondary" size="sm" onclick={submitNewFile}>Add</Button>
        </div>
      {/if}
      {#if files.length === 0 && !addingFile}
        <div class="text-xs text-default-400 italic">No bundled files.</div>
      {/if}
      {#each files as f (f)}
        {@const isOpen = openFileName === f}
        <div class="rounded-medium overflow-hidden bg-surface-inset">
          <!-- The whole padded header is the toggle; the delete button stops the
               click so it doesn't also expand/collapse. -->
          <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
          <div
            class="flex items-center gap-2 px-3 py-2 hov:cursor-pointer"
            role="button"
            tabindex="0"
            onclick={() => (isOpen ? (onCloseFile ?? noop)() : (onOpenFile ?? noop)(f))}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                isOpen ? (onCloseFile ?? noop)() : (onOpenFile ?? noop)(f);
              }
            }}
          >
            <i class="i-material-symbols-description-outline-rounded text-default-500 shrink-0"></i>
            <span class="text-xs font-mono truncate flex-1">{f}</span>
            {#if editable}
              <IconButton
                icon="i-material-symbols-delete-outline-rounded"
                title="Delete file"
                size="xs"
                variant="subtle"
                onclick={(e) => {
                  e.stopPropagation();
                  (onDeleteFile ?? noop)(f);
                }}
              />
            {/if}
          </div>
          <Expand open={isOpen}>
            <div class="px-2 pb-2">
              <Card variant="default" padding="sm">
                <Textarea
                  ariaLabel="Bundled file content"
                  autoResize="none"
                  surface="transparent"
                  mono
                  class="loca-scroll min-h-32 max-h-80 w-full overflow-y-auto resize-y text-xs"
                  value={openFileContent}
                  placeholder={openFileLoaded ? "" : "Loading..."}
                  disabled={!openFileLoaded || !editable}
                  oninput={(v) => (onFileContentInput ?? noop)(v)}
                  onblur={() => (onFileContentBlur ?? noop)()}
                />
              </Card>
            </div>
          </Expand>
        </div>
      {/each}
    </div>
  {/if}
</div>
