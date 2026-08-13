<script lang="ts">
  import SidebarItem from "@loca/shared/ui/components/primitives/SidebarItem.svelte";
  import UpdateButtonView from "@loca/shared/ui/components/settings/UpdateButtonView.svelte";

  // The settings sidebar footer the app pins to the bottom of every settings
  // panel: a Downloads row, then the update/version row. The version row is the
  // SHARED UpdateButtonView the client renders (idle phase), so its markup can
  // never drift from the app across renditions (collapsed, mobile card vs
  // desktop flush row). Server-status chips only show on error/loading, so a
  // healthy default renders none. This is the SINGLE static copy the website
  // uses everywhere it renders the settings UI (the manual demos and the
  // homepage showcase), so the footer cannot be present in one rendition and
  // missing from another. The boolean mirrors the sidebar's collapsed state, so
  // rows collapse to icons with it.
  // `pending` and `downloading` mirror the client's DownloadsButton states:
  // pending shows a "Pending Downloads" label with an accent-yellow ping (files
  // await approval); downloading shows the self-animating loop icon, a
  // "Downloading..." label, and a neutral ping. The ping is driven by `blink`,
  // which the host toggles. Both off by default so every other settings demo
  // shows the idle Downloads row.
  const noop = (): void => {};
  let {
    collapsed = false,
    pending = false,
    downloading = false,
    blink = false,
  }: {
    collapsed?: boolean;
    pending?: boolean;
    downloading?: boolean;
    blink?: boolean;
  } = $props();

  const label = $derived(
    pending ? "Pending Downloads" : downloading ? "Downloading..." : "Downloads",
  );
  const icon = $derived(
    downloading ? "i-line-md-downloading-loop" : "i-material-symbols-downloading-rounded",
  );
</script>

<SidebarItem
  {icon}
  {label}
  {collapsed}
  ping={(pending || downloading) && blink}
  pingTone={pending ? "accent" : "default"}
  title={collapsed ? label : undefined}
  ariaLabel={label}
  onclick={noop}
/>
<UpdateButtonView phase="idle" {collapsed} />
