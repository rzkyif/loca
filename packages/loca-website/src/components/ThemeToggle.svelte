<script lang="ts">
  // Navbar chrome (not a represented app component): an icon button in the bar
  // (icon variant, the shared IconButton at navbar `xl` scale, matching the
  // GitHub + burger icons), a labeled SidebarItem-style row (row variant), or a
  // filled card button for the mobile menu footer (card variant, sitting beside
  // the GitHub card). Toggles the `.dark` class the no-flash head script set, and
  // persists the choice. With JS off this control is absent and the theme follows
  // the OS.
  import IconButton from "@loca/shared/ui/components/primitives/IconButton.svelte";

  let { variant = "icon" }: { variant?: "icon" | "row" | "card" } = $props();

  let dark = $state(false);

  $effect(() => {
    dark = document.documentElement.classList.contains("dark");
  });

  function toggle() {
    dark = !dark;
    document.documentElement.classList.toggle("dark", dark);
    try {
      localStorage.setItem("loca-theme", dark ? "dark" : "light");
    } catch (_) {
      // private mode / storage disabled: the toggle still works for the session
    }
  }

  const icon = $derived(
    dark ? "i-material-symbols-light-mode-rounded" : "i-material-symbols-dark-mode-rounded",
  );
  const title = $derived(dark ? "Switch to light theme" : "Switch to dark theme");
</script>

{#if variant === "card"}
  <button
    type="button"
    onclick={toggle}
    {title}
    aria-label={title}
    class="flex flex-1 items-center justify-center gap-2 h-10 px-4 rounded-medium bg-surface-inset text-default-700 hover:bg-surface-inset-strong hover:text-default-900 transition-colors hover:cursor-pointer"
  >
    <i class="flex text-lg shrink-0 {icon}"></i>
    <span class="flex-1 text-center">{dark ? "Light Theme" : "Dark Theme"}</span>
    <i class="flex text-lg shrink-0 {icon} invisible" aria-hidden="true"></i>
  </button>
{:else if variant === "row"}
  <button
    type="button"
    onclick={toggle}
    {title}
    aria-label={title}
    class="flex items-center h-8 pl-1.5 pr-2.5 gap-1.5 rounded-medium text-default-500 hover:text-default-700 hover:bg-surface-inset transition-colors hover:cursor-pointer"
  >
    <i class="flex text-xl shrink-0 {icon}"></i>
    {dark ? "Light Theme" : "Dark Theme"}
  </button>
{:else}
  <IconButton size="xl" {icon} {title} onclick={toggle} />
{/if}
