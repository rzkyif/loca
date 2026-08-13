<script lang="ts">
  // Presentational body of a snippet's detail pane: the name field, the trigger
  // select (each option is the full live trigger preview, e.g. `@name`), the
  // placement select, and the snippet text textarea. All values arrive
  // pre-resolved (the client owns the store, the draft snapshot, the name
  // validation, and the symbol options whose labels are the previews), so this
  // stays pure: props in, callbacks out. The detail header and scroll shell live in ../objects/*;
  // this is only SnippetDetail's own body markup. The four draft fields are
  // $bindable to mirror the client's edit drafts.
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Select from "../primitives/Select.svelte";
  import Textarea from "../primitives/Textarea.svelte";

  type Option = { value: string; label: string };

  let {
    nameError = null,
    symbolOptions = [],
    placementOptions = [],
    onNameInput,
    onNameBlur,
    onSymbolChange,
    onPlacementChange,
    onTextInput,
    onTextBlur,
    draftName = $bindable(""),
    draftSymbol = $bindable("#"),
    draftPlacement = $bindable(""),
    draftText = $bindable(""),
  }: {
    nameError?: string | null;
    symbolOptions?: Option[];
    placementOptions?: Option[];
    onNameInput?: (v: string) => void;
    onNameBlur?: () => void;
    onSymbolChange?: (v: string) => void;
    onPlacementChange?: (v: string) => void;
    onTextInput?: (v: string) => void;
    onTextBlur?: () => void;
    draftName?: string;
    draftSymbol?: string;
    draftPlacement?: string;
    draftText?: string;
  } = $props();

  const noop = (): void => {};
</script>

<div class="flex flex-col gap-3">
  <FormField label="Name">
    <Input
      type="text"
      value={draftName}
      placeholder="scientist"
      ariaLabel="Snippet name"
      mono
      error={!!nameError}
      oninput={(v) => (onNameInput ?? noop)(v)}
      onblur={() => (onNameBlur ?? noop)()}
    />
  </FormField>

  <FormField label="Trigger" error={nameError}>
    <Select
      value={draftSymbol}
      options={symbolOptions}
      ariaLabel="Snippet trigger symbol"
      error={!!nameError}
      onchange={(v) => (onSymbolChange ?? noop)(v)}
    />
  </FormField>

  <FormField label="Placement">
    <Select
      value={draftPlacement}
      options={placementOptions}
      ariaLabel="Snippet placement"
      onchange={(v) => (onPlacementChange ?? noop)(v)}
    />
  </FormField>

  <FormField label="Text">
    <Textarea
      ariaLabel="Snippet text"
      autoResize="none"
      class="max-h-64 min-h-24 overflow-y-auto resize-none"
      value={draftText}
      oninput={(v) => (onTextInput ?? noop)(v)}
      onblur={() => (onTextBlur ?? noop)()}
    />
  </FormField>
</div>
