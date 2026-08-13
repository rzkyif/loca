<script lang="ts">
  import { onMount } from "svelte";
  import { confirmState, extensionsState, settingsState, viewState } from "../../state";
  import { useUiContext } from "@loca/shared/ui/context";
  import Modal from "@loca/shared/ui/components/primitives/Modal.svelte";
  import Button from "@loca/shared/ui/components/primitives/Button.svelte";

  // Offer to install the built-in tools each time the user turns tools on while
  // the built-in is present-but-not-installed, until they install it or choose
  // "Don't Ask Again". The built-in is seeded to 'downloaded' offline on first
  // boot; this prompt drives the explicit install.
  //
  // Mounted inside the panel that hosts the Tools toggle (Settings and Quick
  // Settings), within that panel's `relative` bubble wrapper, so the Modal's
  // backdrop stays clipped to the panel (not the whole viewport) and, on the
  // desktop overlay, lives under the click-through content element so its
  // buttons register as real UI instead of passing the click to the desktop
  // behind. `surface` names which panel hosts this instance so it only reacts to
  // a toggle made on the surface the user is actually looking at (on mobile both
  // panels can be mounted at once).

  let { surface }: { surface: "settings" | "quickSettings" } = $props();

  const ui = useUiContext();
  const mobile = $derived(ui.platform === "mobile");

  let open = $state(false);
  let installing = $state(false);

  // Fire only on the user's own enable action (origin "user"), not on the
  // baseline load / remote sync of tools.enabled (which would misfire on app
  // start when tools are already on). applyChanges already drops no-op
  // transitions, so `next === true` means it just flipped on.
  onMount(() =>
    settingsState.onChange((key, _prev, next, origin) => {
      if (key !== "tools.enabled" || origin !== "user" || next !== true) return;
      if (viewState.mode !== surface) return;
      if (settingsState.currentSettings["tools.builtinPromptDismissed"]) return;
      // Present but not installed. Absent (user deleted it) or already installed
      // => no prompt.
      if (!extensionsState.isBuiltinPendingInstall) return;
      open = true;
    }),
  );

  async function install() {
    if (installing) return;
    installing = true;
    try {
      const { queued } = await extensionsState.installBuiltinToolkit();
      open = false;
      if (queued) {
        // The worker runtime isn't downloaded yet, so the install can't run now.
        // It will start on its own once those files are in place.
        confirmState.request({
          alert: true,
          title: "Built-in Tools Pending",
          message:
            "Loca will finish installing the built-in tools once the required files for tools are downloaded.",
          confirmLabel: "Got It",
          onConfirm: () => {},
        });
      }
    } finally {
      installing = false;
    }
  }

  function notNow() {
    open = false;
  }

  async function dontAskAgain() {
    open = false;
    await settingsState.updateSetting("tools.builtinPromptDismissed", true);
  }
</script>

{#if open}
  <Modal
    open
    onclose={notNow}
    positioning={mobile ? "fixed" : "absolute"}
    ariaLabel="Install the built-in tools"
  >
    <div class="text-default-800 font-medium">Install Built-in Tools?</div>
    <div class="text-default-600 text-sm">
      Loca ships a built-in Extension with tools like web search and file access. Install it so the
      agent can use the tools you turn on.
    </div>
    <div class="flex items-center justify-end gap-2">
      <Button variant="ghost" class="mr-auto" onclick={dontAskAgain}>Don't Ask Again</Button>
      <Button variant="secondary" onclick={notNow}>Not Now</Button>
      <Button variant="primary" loading={installing} onclick={install}>Install</Button>
    </div>
  </Modal>
{/if}
