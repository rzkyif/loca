<script lang="ts">
  type AutoResize = "none" | "scroll" | "grid";
  type Surface = "default" | "transparent";

  let {
    value,
    oninput,
    onchange,
    onfocus,
    onblur,
    onkeydown,
    onkeyup,
    onclick,
    onpaste,
    oncompositionstart,
    oncompositionend,
    placeholder,
    disabled = false,
    error = false,
    autoResize = "scroll",
    surface = "default",
    minHeight = "min-h-40",
    rows,
    cols,
    mono = false,
    spellcheck,
    autocomplete,
    autocapitalize,
    ariaLabel,
    class: extraClass = "",
    el = $bindable<HTMLTextAreaElement | undefined>(undefined),
  }: {
    value: string;
    oninput?: (v: string, e: Event) => void;
    onchange?: (v: string, e: Event) => void;
    onfocus?: (e: FocusEvent) => void;
    onblur?: (e: FocusEvent) => void;
    onkeydown?: (e: KeyboardEvent) => void;
    onkeyup?: (e: KeyboardEvent) => void;
    onclick?: (e: MouseEvent) => void;
    onpaste?: (e: ClipboardEvent) => void;
    oncompositionstart?: (e: CompositionEvent) => void;
    oncompositionend?: (e: CompositionEvent) => void;
    placeholder?: string;
    disabled?: boolean;
    error?: boolean;
    autoResize?: AutoResize;
    surface?: Surface;
    minHeight?: string;
    rows?: number;
    cols?: number;
    mono?: boolean;
    spellcheck?: boolean;
    autocomplete?: "on" | "off";
    autocapitalize?: "none" | "off" | "on" | "words" | "characters" | "sentences";
    ariaLabel?: string;
    class?: string;
    el?: HTMLTextAreaElement | undefined;
  } = $props();

  let focused = $state(false);

  function fitToContent() {
    if (!el || autoResize !== "scroll") return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }

  function handleFocus(e: FocusEvent) {
    focused = true;
    if (autoResize === "scroll") fitToContent();
    onfocus?.(e);
  }

  function handleBlur(e: FocusEvent) {
    focused = false;
    if (autoResize === "scroll" && el) {
      el.style.height = "";
    }
    onblur?.(e);
  }

  function handleInput(e: Event) {
    const v = (e.target as HTMLTextAreaElement).value;
    oninput?.(v, e);
    if (autoResize === "scroll" && focused) fitToContent();
  }

  // The styled scrollbar is baked into the inset surface so every inset textarea
  // scrolls consistently regardless of `autoResize` (a `surface="transparent"`
  // textarea sits on its parent's surface, so the parent supplies the matching
  // scrollbar class instead).
  const stateClass = $derived(
    surface === "transparent"
      ? "bg-transparent outline-none"
      : error
        ? "bg-surface-inset loca-error-ring text-default-800 rounded-medium px-2 py-1.5 outline-none loca-scroll-inset"
        : "bg-surface-inset text-default-800 rounded-medium px-2 py-1.5 outline-none loca-scroll-inset",
  );

  const sizeClass = $derived(
    autoResize === "grid"
      ? "min-w-0 w-full overflow-hidden resize-none whitespace-pre-wrap break-words"
      : autoResize === "scroll"
        ? `${minHeight} w-full resize-y overflow-y-hidden focus:overflow-y-auto whitespace-pre-wrap break-words`
        : "w-full",
  );

  const fontClass = $derived(`text-sm ${mono ? "font-mono" : ""}`);
  const disabledClass = $derived(disabled ? "opacity-60" : "");
</script>

{#if autoResize === "grid"}
  <div class="grid items-end {extraClass}">
    <span
      class="invisible col-start-1 row-start-1 whitespace-pre-wrap break-words {fontClass}"
      aria-hidden="true">{value}{value.endsWith("\n") ? " " : ""}</span
    >
    <textarea
      bind:this={el}
      class="col-start-1 row-start-1 {stateClass} {sizeClass} {fontClass} {disabledClass}"
      aria-label={ariaLabel}
      {placeholder}
      {disabled}
      {rows}
      {cols}
      {spellcheck}
      {autocomplete}
      {autocapitalize}
      {value}
      oninput={handleInput}
      onchange={(e) => onchange?.((e.target as HTMLTextAreaElement).value, e)}
      onfocus={handleFocus}
      onblur={handleBlur}
      {onkeydown}
      {onkeyup}
      {onclick}
      {onpaste}
      {oncompositionstart}
      {oncompositionend}
    ></textarea>
  </div>
{:else}
  <textarea
    bind:this={el}
    class="{stateClass} {sizeClass} {fontClass} {disabledClass} {extraClass}"
    aria-label={ariaLabel}
    {placeholder}
    {disabled}
    {rows}
    {cols}
    {spellcheck}
    {autocomplete}
    {autocapitalize}
    {value}
    oninput={handleInput}
    onchange={(e) => onchange?.((e.target as HTMLTextAreaElement).value, e)}
    onfocus={handleFocus}
    onblur={handleBlur}
    {onkeydown}
    {onkeyup}
    {onclick}
    {onpaste}
    {oncompositionstart}
    {oncompositionend}
  ></textarea>
{/if}
