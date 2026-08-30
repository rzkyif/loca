<script lang="ts">
  import { onMount } from "svelte";
  import gsap from "gsap";
  import { getDefaultSettings } from "@loca/shared/domain/settings/engine";
  import type { DownloadStatus } from "@loca/shared/domain/model";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import DownloadsModalView from "@loca/shared/ui/components/settings/DownloadsModalView.svelte";
  import ConfirmModalView from "@loca/shared/ui/components/settings/ConfirmModalView.svelte";
  import SettingsDemoFooter from "../demos/SettingsDemoFooter.svelte";
  import Cursor from "./Cursor.svelte";
  import { Demo, settingsShowcaseGroups as groups, type StageProps } from "../../lib/showcase";

  let { register }: StageProps = $props();

  const D = getDefaultSettings();

  type Shell = { reset: () => void };
  let shell = $state<Shell>();
  let selectedGroupId = $state("llm");

  // The files the current configuration needs but the Core does not yet have.
  // Until approved they live only in the Pending Downloads modal (the shared
  // ConfirmModalView); approval enqueues them into the live download queue. The
  // entries mirror the client's plan: a model weight and the runtime binary,
  // each with its resolved source and size hint.
  const MODEL_GB = 4.7;
  const pendingDownloads = [
    {
      source: "Qwen/Qwen2.5-7B-Instruct-GGUF",
      title: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
      subtitle: "Qwen/Qwen2.5-7B-Instruct-GGUF",
      sizeText: `${MODEL_GB} GB`,
    },
    {
      source: "binary:llama-server",
      title: "llama-server",
      subtitle: "v0.3.2",
      sizeText: "12 MB",
    },
  ];

  // The live download queue, populated once the user approves. The model weight
  // downloads first, then the runtime.
  type Row = {
    key: string;
    status: DownloadStatus;
    icon: string;
    filename: string;
    title: string;
    sizeText: string;
    progress: number;
    showReveal?: boolean;
  };
  function freshItems(): Row[] {
    return [
      {
        key: "model",
        status: "Downloading",
        icon: "i-material-symbols-psychology-rounded",
        filename: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        title: "Qwen2.5-7B-Instruct-Q4_K_M.gguf (models/Qwen2.5-7B-Instruct-Q4_K_M.gguf)",
        sizeText: `0.9 GB / ${MODEL_GB} GB`,
        progress: 18,
      },
      {
        key: "runtime",
        status: "Pending",
        icon: "i-material-symbols-download-rounded",
        filename: "llama-server",
        title: "llama-server (binaries/llama-server)",
        sizeText: "12 MB",
        progress: 0,
      },
    ];
  }
  // The queue is empty until approval enqueues the files.
  let items = $state<Row[]>([]);
  let confirmOpen = $state(false);
  let modalOpen = $state(false);
  const hasCompleted = $derived(items.some((r) => r.status === "Completed"));

  // The sidebar Downloads row mirrors the client's DownloadsButton: an
  // accent-yellow ping while files await approval (pending), then the animated
  // loop icon + a neutral ping while the approved queue runs (downloading).
  let footerPending = $state(true);
  let footerRunning = $state(false);
  let footerBlink = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();

  function setModelProgress(pct: number): void {
    const p = Math.round(pct);
    const gb = ((p / 100) * MODEL_GB).toFixed(1);
    items = items.map((r) =>
      r.key === "model" ? { ...r, progress: p, sizeText: `${gb} GB / ${MODEL_GB} GB` } : r,
    );
  }
  function completeModel(): void {
    items = items.map((r) =>
      r.key === "model"
        ? { ...r, progress: 100, status: "Completed", sizeText: `${MODEL_GB} GB`, showReveal: true }
        : r.key === "runtime"
          ? { ...r, status: "Downloading" }
          : r,
    );
  }
  function completeRuntime(): void {
    items = items.map((r) =>
      r.key === "runtime" ? { ...r, progress: 100, status: "Completed", showReveal: true } : r,
    );
  }

  // Approval: dismiss the Pending Downloads modal, enqueue the files, and flip
  // the sidebar from pending to the running download-manager state. The client
  // marks the sources approved up front, so the button switches immediately.
  function approve(): void {
    confirmOpen = false;
    items = freshItems();
    footerPending = false;
    footerRunning = true;
  }

  function reset(): void {
    items = [];
    confirmOpen = false;
    modalOpen = false;
    footerPending = true;
    footerRunning = false;
    shell?.reset();
    selectedGroupId = "llm";
  }

  onMount(() => {
    if (!stageEl || !cursorRef) return;
    const demo = new Demo(cursorRef, stageEl);
    demo.placeFrac(0.5, 0.5);

    // Blink the sidebar download ping (the client uses a 500ms useBlink toggle).
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const blinkTl = gsap.timeline({ repeat: -1, paused: reduce });
    blinkTl.call(() => (footerBlink = !footerBlink)).to({}, { duration: 0.5 });

    const tl = gsap.timeline({ paused: true });

    // 1. Open the Pending Downloads modal from the sidebar footer.
    demo.move(tl, '[aria-label="Pending Downloads"]', { duration: 0.9 });
    demo.hover(tl, '[aria-label="Pending Downloads"]', true);
    demo.click(tl, '[aria-label="Pending Downloads"]', () => (confirmOpen = true));
    demo.hover(tl, '[aria-label="Pending Downloads"]', false);
    demo.hold(tl, 1.0);

    // 2. Approve: click "Download", enqueueing the files and flipping the
    //    sidebar to its running state. The Download button is the modal's
    //    primary (last) action.
    demo.move(tl, '[role="dialog"] button:last-of-type', { duration: 0.7 });
    demo.hover(tl, '[role="dialog"] button:last-of-type', true);
    demo.click(tl, '[role="dialog"] button:last-of-type', () => approve());
    demo.hold(tl, 0.5);

    // 3. Open the Downloads panel from the (now running) sidebar footer.
    demo.move(tl, '[aria-label="Downloading..."]', { duration: 0.9 });
    demo.hover(tl, '[aria-label="Downloading..."]', true);
    demo.click(tl, '[aria-label="Downloading..."]', () => (modalOpen = true));
    demo.hover(tl, '[aria-label="Downloading..."]', false);
    demo.hold(tl, 0.5);

    // 4. The model weight downloads.
    const p = { v: 18 };
    tl.to(p, {
      v: 100,
      duration: 3.0,
      ease: "none",
      onUpdate: () => setModelProgress(p.v),
    });
    tl.add(() => completeModel());
    demo.hold(tl, 0.4);

    // 5. The runtime binary follows.
    const q = { v: 0 };
    tl.to(q, {
      v: 100,
      duration: 1.2,
      ease: "none",
      onUpdate: () =>
        (items = items.map((r) => (r.key === "runtime" ? { ...r, progress: Math.round(q.v) } : r))),
    });
    tl.add(() => completeRuntime());
    // Both files done: the sidebar row returns to its idle Downloads state.
    tl.add(() => (footerRunning = false));

    register({
      timeline: tl,
      reset: () => {
        reset();
        tl.pause(0);
        demo.blur();
        demo.placeFrac(0.5, 0.5);
      },
    });
    return () => {
      tl.kill();
      blinkTl.kill();
    };
  });
</script>

{#snippet sidebarFooter(collapsed: boolean)}
  <SettingsDemoFooter
    {collapsed}
    pending={footerPending}
    downloading={footerRunning}
    blink={footerBlink}
  />
{/snippet}

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <!-- The panel sits in its own positioned box so the (absolute) downloads modal
       overlays just the settings panel, not the whole page. -->
  <div class="relative shrink-0 w-[620px] h-[960px]">
    <SettingsShellView
      bind:this={shell}
      {groups}
      bind:selectedGroupId
      sizeClass="w-[620px] h-[960px]"
      {sidebarFooter}
    >
      {#snippet groupContent(gid)}
        <SettingsContentView groupId={gid} values={D} />
      {/snippet}
    </SettingsShellView>
    <DownloadsModalView
      open={modalOpen}
      {items}
      {hasCompleted}
      onClose={() => (modalOpen = false)}
    />
    <ConfirmModalView
      open={confirmOpen}
      title="Pending Downloads"
      message="The following files need to be downloaded so the Core can run with the current configuration."
      downloads={pendingDownloads}
      totalText={`Total: ${MODEL_GB} GB`}
      confirmLabel="Download"
      cancelLabel="Do It Later"
      onConfirm={approve}
      onCancel={() => (confirmOpen = false)}
      onClose={() => (confirmOpen = false)}
    />
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
