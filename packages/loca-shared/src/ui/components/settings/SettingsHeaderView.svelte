<script lang="ts">
  import IconButton from "../primitives/IconButton.svelte";
  import SearchInput from "../primitives/SearchInput.svelte";
  import { useUiContext } from "../../context.ts";

  const ui = useUiContext();
  // The close (X) is dropped only where the OS owns back navigation (Android):
  // the screen is dismissed with the system back there. Desktop and iOS keep it.
  const showClose = !ui.hasSystemBack;

  // THE single settings panel header for both the client and the website
  // (single-source rule, AGENTS.md): the search box plus the Quick Settings,
  // Import/Export, and Back-to-Chat buttons. Both sides wrap it; the client feeds
  // live search + real handlers, the website feeds noop handlers.
  const noop = (): void => {};

  let {
    searchValue = $bindable(""),
    searchEl = $bindable<HTMLInputElement | undefined>(undefined),
    searchPlaceholder = "Search settings...",
    searchAriaLabel = "Search settings",
    searchDisabled = false,
    onSearchInput,
    onSearchFocus,
    onSearchClear,
    onQuickSettings = noop,
    onShare = noop,
    onClose = noop,
    class: extraClass = "",
  }: {
    searchValue?: string;
    searchEl?: HTMLInputElement;
    searchPlaceholder?: string;
    searchAriaLabel?: string;
    searchDisabled?: boolean;
    onSearchInput?: (v: string) => void;
    onSearchFocus?: (e: FocusEvent) => void;
    onSearchClear?: () => void;
    onQuickSettings?: () => void;
    onShare?: () => void;
    onClose?: () => void;
    class?: string;
  } = $props();
</script>

<div class="flex gap-2 items-center text-2xl {extraClass}">
  <SearchInput
    bind:value={searchValue}
    bind:el={searchEl}
    placeholder={searchPlaceholder}
    ariaLabel={searchAriaLabel}
    disabled={searchDisabled}
    oninput={(v) => onSearchInput?.(v)}
    onfocus={(e) => onSearchFocus?.(e)}
    onclear={() => onSearchClear?.()}
  />
  <IconButton
    icon="i-material-symbols-bolt-rounded"
    title="Quick Settings"
    size="lg"
    variant="subtle"
    surface="circle"
    onclick={onQuickSettings}
  />
  <IconButton
    icon="i-material-symbols-ios-share-rounded"
    title="Import / Export Settings"
    size="lg"
    variant="subtle"
    surface="circle"
    onclick={onShare}
  />
  {#if showClose}
    <IconButton
      icon="i-material-symbols-close-rounded"
      title="Back to Chat"
      size="lg"
      variant="subtle"
      surface="circle"
      onclick={onClose}
    />
  {/if}
</div>
