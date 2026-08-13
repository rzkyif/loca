<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { useUiContext } from "../../context.ts";
  import { RIPPLE_MS } from "../../animations.ts";
  import { ripple } from "../../actions/ripple.ts";

  type Size = "xs" | "sm" | "md" | "lg" | "lg-tight" | "xl";
  type Variant = "default" | "subtle";
  type Surface = "none" | "filled" | "circle";

  type Props = Omit<
    HTMLButtonAttributes,
    "class" | "type" | "disabled" | "onclick" | "title" | "aria-label"
  > & {
    /** Icon class string (e.g. "i-material-symbols-add-rounded") or a
     *  snippet for arbitrary inner content (custom SVG, animated indicator,
     *  conditional icon trees, etc.). Omit it and pass `children` instead when
     *  the content comes from a slot (e.g. an Astro host). */
    icon?: string | Snippet;
    title: string;
    ariaLabel?: string;
    size?: Size;
    variant?: Variant;
    surface?: Surface;
    /** Override the surface's default corner radius (e.g. "rounded-large" to
     *  match an adjacent search bar). Defaults to the per-surface radius. */
    rounded?: string;
    active?: boolean;
    /** Override the text-color classes entirely (e.g. an accent-yellow ping).
     *  When set, replaces the variant's default color logic. */
    colorClass?: string;
    disabled?: boolean;
    type?: "button" | "submit";
    onclick?: (e: MouseEvent) => void;
    badge?: Snippet;
    /** Slot content used as the inner icon when `icon` is not supplied (lets an
     *  Astro host pass a masked logo or animated bars as default-slot children). */
    children?: Snippet;
    /** Render as a link (`<a href>`) instead of a `<button>`. */
    href?: string;
    target?: string;
    rel?: string;
    /** Render as a `<label for>` instead of a `<button>` (e.g. a CSS-only
     *  checkbox toggle like the mobile menu burger). */
    forId?: string;
    class?: string;
  };

  let {
    icon,
    title,
    ariaLabel,
    size = "md",
    variant = "default",
    surface = "none",
    rounded,
    active = false,
    colorClass,
    disabled = false,
    type = "button",
    onclick,
    badge,
    children,
    href,
    target,
    rel,
    forId,
    class: extraClass = "",
    ...rest
  }: Props = $props();

  const sizeClass = $derived(
    {
      xs: "w-5 h-5 text-sm",
      sm: "p-0.5 text-lg",
      md: "p-1 text-lg",
      lg: "p-2 text-xl",
      // Same lg icon (text-xl) but half the padding, for buttons packed into a
      // ButtonGroup-style pill that carries the other half as its own padding.
      "lg-tight": "p-1 text-xl",
      // Navbar scale: a larger icon centered in a 36px box (minimal padding),
      // matching the mobile burger button's footprint.
      xl: "w-9 h-9 text-[1.75rem]",
    }[size],
  );

  // A coarse pointer (touch) can't hover, so the `subtle` resting tone can't
  // rely on `hov:` to brighten: it would sit permanently at the dim shade. Rest
  // it at the hover shade instead (press feedback comes from the ripple). The
  // default variant already rests at a readable shade, so only `subtle` shifts.
  const ui = useUiContext();
  const variantClass = $derived(
    colorClass ??
      (variant === "subtle"
        ? active || ui.pointer === "coarse"
          ? "text-default-700"
          : "text-default-400 hov:text-default-700"
        : active
          ? "text-default-900"
          : "text-default-700 hov:text-default-900"),
  );

  // Hover background follows the shared interaction standard; the press splash
  // is the shared `use:ripple` action, not a color shift. A surfaceless icon
  // button materializes the inset surface on hover (the text-darken in
  // `variantClass` is a complementary cue); a filled/circle button steps its
  // existing fill one shade on hover. An `active` surfaceless button already
  // carries its own toggled-on fill (supplied via `class`), so it drops the
  // hover inset rather than layering one on top.
  const surfaceClass = $derived(
    {
      none: active ? "" : "hov:bg-surface-inset",
      filled: "bg-surface-inset hov:bg-surface-inset-strong",
      circle: "bg-surface-inset hov:bg-surface-inset-strong",
    }[surface],
  );

  const rippleDuration = $derived(ui.animationDurationMs(RIPPLE_MS));

  const roundedClass = $derived(
    rounded ??
      {
        none: "rounded",
        filled: "rounded-medium",
        circle: "rounded-large",
      }[surface],
  );

  const cls = $derived(
    `flex items-center justify-center shrink-0 ${sizeClass} ${variantClass} ${surfaceClass} ${roundedClass} hov:cursor-pointer transition-interactive disabled:opacity-50 disabled:pointer-events-none ${extraClass}`,
  );
  const label = $derived(ariaLabel ?? title);
</script>

{#snippet inner()}
  {#if badge}
    <span class="relative flex shrink-0">
      {#if typeof icon === "string"}
        <i class="flex {icon}"></i>
      {:else if icon}
        {@render icon()}
      {:else if children}
        {@render children()}
      {/if}
      {@render badge()}
    </span>
  {:else if typeof icon === "string"}
    <i class="flex {icon}"></i>
  {:else if icon}
    {@render icon()}
  {:else if children}
    {@render children()}
  {/if}
{/snippet}

{#if href}
  <a
    {href}
    {target}
    {rel}
    {title}
    aria-label={label}
    class={cls}
    {onclick}
    use:ripple={{ disabled, durationMs: rippleDuration }}
  >
    {@render inner()}
  </a>
{:else if forId}
  <label
    for={forId}
    {title}
    aria-label={label}
    class={cls}
    use:ripple={{ disabled, durationMs: rippleDuration }}
  >
    {@render inner()}
  </label>
{:else}
  <button
    {...rest}
    {type}
    {disabled}
    {title}
    aria-label={label}
    {onclick}
    class={cls}
    use:ripple={{ disabled, durationMs: rippleDuration }}
  >
    {@render inner()}
  </button>
{/if}
