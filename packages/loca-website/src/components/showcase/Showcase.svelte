<script lang="ts">
  import { onMount } from "svelte";
  import { SvelteMap } from "svelte/reactivity";
  import gsap from "gsap";
  import ChatStage from "./ChatStage.svelte";
  import SpeechStage from "./SpeechStage.svelte";
  import LocalFirstStage from "./LocalFirstStage.svelte";
  import DualModelStage from "./DualModelStage.svelte";
  import ThemeStage from "./ThemeStage.svelte";
  import DownloadStage from "./DownloadStage.svelte";
  import MemoryStage from "./MemoryStage.svelte";
  import McpStage from "./McpStage.svelte";
  import CustomToolStage from "./CustomToolStage.svelte";
  import PermissionStage from "./PermissionStage.svelte";
  import GreetingStage from "./GreetingStage.svelte";
  import ScheduleStage from "./ScheduleStage.svelte";
  import MobileStage from "./MobileStage.svelte";
  import { makeUiContext, setUiContext } from "@loca/shared/ui/context";
  import { getDefaultSettings } from "@loca/shared/domain/settings/engine";
  import { BASE_MS } from "@loca/shared/ui/animations";
  import { type Timeline } from "../../lib/showcase";
  import ShowcaseFrame from "./ShowcaseFrame.svelte";

  // Full-width horizontal snap-scroll track. Each panel is capped to the content
  // width (the Get Loca button width) and holds a scale-to-fit stage
  // (ShowcaseFrame) above its caption; neighbours peek on wide screens, dimmed and
  // frozen at time 0, while only the centred stage animates.
  type Handle = { timeline: Timeline; reset: () => void };

  // Content width (the Get Loca button / caption width, max-w-xl) that caps every
  // panel, and the gap between adjacent panels (part of the snap step).
  const CONTENT_W = 576;
  const GAP = 48;

  // A showcase caption. `desc` is plain text; `descHtml` (used by the local-first
  // stage) carries a small, controlled HTML string so the description can hold
  // inline links to the upstream projects. The links sit above the panel's
  // play/pause cover (relative z-30) so they stay clickable during autoplay.
  const meta: { title: string; desc?: string; descHtml?: string }[] = [
    {
      title: "Loca is an open-source AI assistant",
      desc: "Press the shortcut key and it will show up as an overlay on top of other applications for quick access. Designed to feel like an integrated AI assistant.",
    },
    {
      title: "End-to-end speech capabilities",
      desc: "Speech-to-Text lets you talk to the assistant, Text-to-Speech lets the assistant speak to you.",
    },
    {
      title: "Local-first for privacy and security",
      descHtml:
        'Powered by <a class="relative z-30 underline transition-interactive hov:text-default-900" href="https://github.com/ggml-org/llama.cpp" target="_blank" rel="noreferrer">llama.cpp</a> and <a class="relative z-30 underline transition-interactive hov:text-default-900" href="https://github.com/k2-fsa/sherpa-onnx" target="_blank" rel="noreferrer">sherpa-onnx</a> to provide a wide selection of local language and speech models. Runs completely off the internet, while still allowing you to use external providers if you want.',
    },
    {
      title: "Dual model routing",
      desc: "Set it up to use a small model locally while redirecting more complex requests to a more powerful external model.",
    },
    {
      title: "Complete themeability",
      desc: "Change the colors, fonts, shapes, effects, and more, because why not?",
    },
    {
      title: "Built-in downloader",
      desc: "Everything you need to start is automatically listed along with the file sizes and downloaded upon approval.",
    },
    {
      title: "Knowledges and skills",
      desc: "Set up the assistant with extra information on things and how to do things, and it will automatically recall the most relevant ones on every prompt.",
    },
    {
      title: "Model Context Protocol support",
      desc: "Plug in popular MCP servers to quickly give the assistant access to various external services.",
    },
    {
      title: "Highly extensible via Extensions",
      descHtml:
        'For things super specific to your use case, write your own simple tool in JavaScript or TypeScript with our <a class="relative z-30 underline transition-interactive hov:text-default-900" href="/manual/tools/extensions">Extensions system</a>.',
    },
    {
      title: "Sandboxed tool execution",
      descHtml: `Tools from extensions are run on the secure-by-default <a class="relative z-30 underline transition-interactive hov:text-default-900" href="https://deno.com/" target="_blank" rel="noreferrer">deno</a> runtime and makes full use of its granular permission system.`,
    },
    {
      title: "Automated greetings system",
      desc: "Set it up to automatically greet you when your PC starts, or everytime the app is opened.",
    },
    {
      title: "Scheduled prompting",
      desc: "Make the agent do regular or repetitive tasks automatically on a schedule.",
    },
    {
      title: "Control it from your mobile device",
      desc: "The mobile client allows you to securely communicate with your desktop assistant remotely, giving full access to all of its capabilities.",
    },
  ];

  const STAGES = 13;
  const handles: (Handle | undefined)[] = Array.from({ length: STAGES }, () => undefined);
  let registered = 0;
  let ready = $state(false);

  let index = $state(0);
  let progress = $state(0);
  let scrolling = $state(false);
  // Active stage manually paused by clicking its cover; cleared on settle.
  let paused = $state(false);
  let trackW = $state(0);
  // Scroll position in stage units (0.5 = halfway); drives the live dim + bar morph.
  let scrollFrac = $state(0);

  // Panel width: the content width, capped to the track on screens narrower than
  // it (so a phone panel never overflows). The snap step adds the inter-panel gap.
  const panelW = $derived(trackW > 0 ? Math.min(trackW, CONTENT_W) : CONTENT_W);
  const step = $derived(panelW + GAP);
  // Centre a content-width panel in the full-width track.
  const padInline = $derived(Math.max(0, (trackW - panelW) / 2));
  // Soft-fade the track edges only when neighbours peek (panel narrower than the
  // track). On a phone the panel fills the width with no peek, so the fade would
  // dim the active demo's own edges instead of a peeking neighbour.
  const edgeFade = $derived(trackW > panelW);
  let sectionEl: HTMLElement | undefined = $state();

  // Closeness of stage `i` to the current scroll centre, 0..1.
  function weight(i: number): number {
    return Math.max(0, 1 - Math.abs(scrollFrac - i));
  }

  // Cosmetic fill (0..100) for stage `i`'s playback segment, interpolated from
  // the live scroll position so a segment animates smoothly between an empty dot
  // (0), the centred progress bar (its live progress), and a full dot (100) as
  // the centre slides past it: scrolling right carries the leaving stage from its
  // current progress up to full, scrolling left carries both the leaving stage
  // and the arriving one down to empty. Purely visual; the GSAP timelines still
  // pause on scroll and reset on settle exactly as before (this only READS
  // `progress` as the centre anchor, it never drives it).
  function fillPct(i: number): number {
    // Where the segment rests when it is the centre: the active stage shows its
    // live progress, every other stage restarts from empty once centred.
    const centre = (i === index ? progress : 0) * 100;
    const d = scrollFrac - i;
    if (d >= 1) return 100; // fully left of centre: played, full
    if (d <= -1) return 0; // fully right of centre: upcoming, empty
    if (d >= 0) return centre + (100 - centre) * d; // centre -> played
    return centre * (d + 1); // upcoming -> centre
  }

  let trackEl: HTMLElement | undefined = $state();
  // Reactive so the play effect and context recompute once onMount reads the query.
  let reduce = $state(false);
  let scrollSettleTimer = 0;
  // After a stage's timeline finishes, the orchestrator (not the stage) holds a
  // fixed still-frame before auto-advancing, so the end-of-demo pause is identical
  // across every showcase regardless of its own pacing. `waiting` is that hold; the
  // timer is kept OUT of the stage timelines and paused alongside playback.
  let waiting = $state(false);
  let advanceTimer = 0;
  const ADVANCE_DELAY_MS = 2000;

  // Shared UI context for the subtree: schema defaults, but animations collapse to 0
  // under reduced motion so previews jump to the final frame with no movement.
  const showcaseDefaults = getDefaultSettings();
  // A reactive expansion registry so a stage can collapse/expand a shared
  // Expandable bubble (e.g. the relevant-tools card) from its timeline by id.
  const expansion = new SvelteMap<string, boolean>();
  setUiContext(
    makeUiContext({
      // Force Speech-to-Text on for the showcases so every composer renders the
      // Voice Input button (it ships off by default, which would hide the mic and
      // break the speech demo). Everything else follows the schema defaults.
      getSetting: (key) => (key === "stt.enabled" ? true : showcaseDefaults[key]),
      animationDurationMs: (ms = BASE_MS) => (reduce ? 0 : ms),
      expansionGet: (id, fallback = false) =>
        id !== undefined ? (expansion.get(id) ?? fallback) : fallback,
      expansionSet: (id, value) => expansion.set(id, value),
      expansionInit: (id, value) => {
        if (!expansion.has(id)) expansion.set(id, value);
      },
    }),
  );

  // The active timeline finished: begin the fixed inter-showcase wait (not an
  // immediate advance), so every demo holds its final frame for the same beat.
  function onComplete(): void {
    if (reduce) return;
    waiting = true;
    armAdvance();
  }

  // Arm (or cancel) the post-animation wait. Re-run whenever pause / scroll state
  // changes so the countdown holds while paused or scrolling, then restarts fresh
  // once playback is settled again.
  function armAdvance(): void {
    clearTimeout(advanceTimer);
    if (!waiting || paused || scrolling || reduce) return;
    advanceTimer = window.setTimeout(() => {
      waiting = false;
      scrollToIndex((index + 1) % STAGES);
    }, ADVANCE_DELAY_MS);
  }

  function register(i: number, h: Handle): void {
    handles[i] = h;
    h.timeline.eventCallback("onComplete", onComplete);
    if (++registered === STAGES) {
      // Lock every stage to time 0; the play effect starts the active one.
      handles.forEach((x) => x?.reset());
      ready = true;
    }
  }

  // Reveal the homepage hero once every stage has measured and registered (so the
  // layout has stopped settling). Flipping `main[data-ready]` triggers the staged
  // fade in site.css: the install button fades in, then the showcase scrim fades
  // out. All the load-time settling (scale, scroll, height) happens behind that,
  // so it is never seen as a shift.
  $effect(() => {
    if (ready) sectionEl?.closest("main")?.setAttribute("data-ready", "");
  });

  // Play the active timeline only when settled; reduced motion jumps to the end.
  $effect(() => {
    if (!ready) return;
    const tl = handles[index]?.timeline;
    if (!tl) return;
    if (reduce) {
      tl.progress(1);
      return;
    }
    if (scrolling || paused) tl.pause();
    else tl.play();
  });

  // Hold or resume the post-animation wait alongside the playback state, so the
  // 3s still-frame pauses with the demo and never fires while scrolling.
  $effect(() => {
    void paused;
    void scrolling;
    void waiting;
    armAdvance();
  });

  // Toggle play/pause on the active stage (clicking its own cover).
  function togglePause(): void {
    paused = !paused;
  }

  function lockAndStart(): void {
    waiting = false;
    handles.forEach((h, j) => {
      if (j !== index) h?.reset();
    });
    handles[index]?.reset();
    // The play effect resumes the active stage now that scrolling is false.
  }

  function onScroll(): void {
    if (!trackEl) return;
    scrollFrac = step ? trackEl.scrollLeft / step : 0;
    if (!scrolling) {
      scrolling = true;
      handles[index]?.timeline.pause();
    }
    clearTimeout(scrollSettleTimer);
    scrollSettleTimer = window.setTimeout(settle, 120);
  }

  function settle(): void {
    if (!trackEl) return;
    const next = Math.max(0, Math.min(STAGES - 1, Math.round(scrollFrac)));
    index = next;
    scrolling = false;
    paused = false;
    lockAndStart();
  }

  function scrollToIndex(i: number): void {
    if (!trackEl) return;
    waiting = false;
    trackEl.scrollTo({
      left: i * step,
      behavior: reduce ? "auto" : "smooth",
    });
  }

  // Desktop drag-to-scroll: press and drag anywhere on the track to pan the
  // stages horizontally. The covers (z-20) sit over the track and would swallow
  // the gesture, so the press is read from document-level mouse listeners (wired
  // in onMount). Once the pointer moves past a small threshold this becomes a
  // pan: snap is dropped so the track follows the pointer freely, and the click
  // that follows the release is suppressed so a drag never toggles pause or jumps.
  let dragging = $state(false);
  let dragActive = false;
  let dragMoved = false;
  let dragStartX = 0;
  let dragStartScroll = 0;
  const DRAG_THRESHOLD = 6;

  function onDragStart(e: MouseEvent): void {
    if (e.button !== 0 || !trackEl || !trackEl.contains(e.target as Node)) return;
    dragActive = true;
    dragMoved = false;
    dragStartX = e.pageX;
    dragStartScroll = trackEl.scrollLeft;
  }

  function onDragMove(e: MouseEvent): void {
    if (!dragActive || !trackEl) return;
    const dx = e.pageX - dragStartX;
    if (!dragMoved && Math.abs(dx) > DRAG_THRESHOLD) {
      dragMoved = true;
      dragging = true;
    }
    if (dragMoved) {
      e.preventDefault();
      trackEl.scrollLeft = dragStartScroll - dx;
    }
  }

  function onDragEnd(): void {
    if (!dragActive) return;
    dragActive = false;
    // Snap re-engages from the released position; the ensuing scroll runs the
    // usual settle().
    dragging = false;
  }

  // Capture-phase: swallow the click that fires after a real drag so the cover
  // underneath never toggles pause / jumps. A plain click never sets dragMoved.
  function onDragClickCapture(e: MouseEvent): void {
    if (!dragMoved || !trackEl?.contains(e.target as Node)) return;
    e.stopPropagation();
    e.preventDefault();
    dragMoved = false;
  }

  onMount(() => {
    reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    // The only layout measurement left: the track width drives the panel width,
    // the snap step, and the centring padding. Everything vertical is plain
    // flexbox, and each stage scales itself to its own bounds (see ShowcaseFrame).
    const ro = new ResizeObserver(() => {
      // Defer the read+write to the next frame so neither feeds back into the
      // observer during dispatch (avoids the benign "ResizeObserver loop" error).
      requestAnimationFrame(() => {
        if (!trackEl) return;
        trackW = trackEl.clientWidth;
        // Keep the active stage centred after a resize changed the snap step.
        trackEl.scrollLeft = index * step;
      });
    });
    if (trackEl) ro.observe(trackEl);
    // Mirror the active timeline's progress onto the active bar each frame.
    const tick = () => {
      const tl = handles[index]?.timeline;
      if (tl) progress = tl.progress();
    };
    gsap.ticker.add(tick);
    // Desktop drag-to-scroll wiring: document-level so a cover can't swallow the
    // press, and a capture-phase click so a post-drag click is killed before any
    // cover's onclick runs.
    window.addEventListener("mousedown", onDragStart);
    window.addEventListener("mousemove", onDragMove);
    window.addEventListener("mouseup", onDragEnd);
    window.addEventListener("click", onDragClickCapture, true);
    return () => {
      gsap.ticker.remove(tick);
      ro.disconnect();
      clearTimeout(advanceTimer);
      clearTimeout(scrollSettleTimer);
      window.removeEventListener("mousedown", onDragStart);
      window.removeEventListener("mousemove", onDragMove);
      window.removeEventListener("mouseup", onDragEnd);
      window.removeEventListener("click", onDragClickCapture, true);
    };
  });
</script>

<section
  bind:this={sectionEl}
  class="showcase-section relative flex-1 min-h-0 w-full flex flex-col gap-4"
>
  <!-- Track + its edge fades. The fades are pointer-events-none gradient
       OVERLAYS painted in front of the track's left/right edges (fading to the
       page bg-surface), NOT a `mask` on the track: a mask on the bubbles'
       ancestor breaks their `backdrop-filter` halo in WebKit, so we paint the
       fade on top instead. They sit above the demos but below nothing they need
       to block (pointer-events-none), and only show when neighbours peek. -->
  <div class="relative w-full flex-1 min-h-0">
    <!-- Stage track: full-width horizontal snap-scroll, one content-width panel
         per snap unit; neighbours peek and dim, only the centred stage animates. -->
    <div
      bind:this={trackEl}
      onscroll={onScroll}
      class="no-scrollbar w-full h-full flex overflow-x-auto overflow-y-hidden overscroll-x-none {dragging
        ? 'cursor-grabbing select-none'
        : 'snap-x snap-mandatory'}"
      style="gap: {GAP}px; padding-inline: {padInline}px"
    >
      {#each meta as m, i (i)}
        <!-- Panel: content-width column, full track height. The stage grows to
             fill the height left after the fixed-height caption, and scales
             itself to fit (see ShowcaseFrame). -->
        <div
          class="relative snap-center shrink-0 h-full flex flex-col gap-6"
          style="width: {panelW}px"
        >
          <ShowcaseFrame>
            {#snippet children(setHeight)}
              {#if i === 0}
                <ChatStage register={(h) => register(0, h)} reportHeight={setHeight} />
              {:else if i === 1}
                <SpeechStage register={(h) => register(1, h)} reportHeight={setHeight} />
              {:else if i === 2}
                <LocalFirstStage register={(h) => register(2, h)} />
              {:else if i === 3}
                <DualModelStage register={(h) => register(3, h)} reportHeight={setHeight} />
              {:else if i === 4}
                <ThemeStage register={(h) => register(4, h)} />
              {:else if i === 5}
                <DownloadStage register={(h) => register(5, h)} />
              {:else if i === 6}
                <MemoryStage register={(h) => register(6, h)} reportHeight={setHeight} />
              {:else if i === 7}
                <McpStage register={(h) => register(7, h)} reportHeight={setHeight} />
              {:else if i === 8}
                <CustomToolStage register={(h) => register(8, h)} reportHeight={setHeight} />
              {:else if i === 9}
                <PermissionStage register={(h) => register(9, h)} reportHeight={setHeight} />
              {:else if i === 10}
                <GreetingStage register={(h) => register(10, h)} reportHeight={setHeight} />
              {:else if i === 11}
                <ScheduleStage register={(h) => register(11, h)} />
              {:else}
                <MobileStage register={(h) => register(12, h)} reportHeight={setHeight} />
              {/if}
            {/snippet}
          </ShowcaseFrame>

          <!-- Caption below the demo, full panel width. On phones it hugs its
             content (no fixed height) so the stage claims as much of the scarce
             vertical space as possible. On wider screens it takes a fixed height
             (sized to the tallest description) so every stage gets identical
             bounds and renders at a consistent size. The caption text has its
             half-leading trimmed (see site.css [data-caption]), so this
             title->description gap and the section's description->bar gap are the
             SAME `gap-4` token AND read as the same visible distance. -->
          <div data-caption class="shrink-0 sm:h-24 w-full px-4 text-left flex flex-col gap-4">
            <h2 class="text-lg font-semibold text-default-900">{m.title}</h2>
            <p class="text-sm text-default-600 text-justify">
              {#if m.descHtml}{@html m.descHtml}{:else}{m.desc}{/if}
            </p>
          </div>

          <!-- Dim overlay: fades an off-centre stage toward the page background. A
             translucent surface-colored scrim PAINTED OVER the panel (a sibling,
             pointer-events-none), NOT `opacity` on the panel: opacity < 1 on the
             bubbles' ancestor establishes a backdrop root in WebKit and kills
             their `backdrop-filter` halo. Worse, a stage born off-centre (opacity
             < 1) has its halo layer established broken and never recovers when it
             later centres, so only the first stage (born centred at opacity 1)
             kept its blur. A scrim isn't an ancestor of any bubble, so every
             stage keeps its halo at all times; the centred stage's scrim is fully
             transparent (the surface-color scrim at 1 - V composites identically
             to the old panel-at-opacity-V, so the dim look is unchanged). -->
          <div class="stage-dim" aria-hidden="true" style="opacity: {0.65 * (1 - weight(i))}"></div>

          <!-- Cover: blocks pointer access to the demo (so the playing demo can't be
             clicked). On a side demo it focuses that stage; on the active demo it
             toggles play/pause of its animation. Transparent; the demo shows through. -->
          <button
            type="button"
            aria-label={i === index
              ? `${paused ? "Play" : "Pause"}: ${m.title}`
              : `Show: ${m.title}`}
            class="absolute inset-0 z-20 {dragging ? 'cursor-grabbing' : 'cursor-pointer'}"
            onclick={() => (i === index ? togglePause() : scrollToIndex(i))}
          ></button>
        </div>
      {/each}
    </div>

    <!-- Edge fades: pointer-events-none gradient overlays in front of the
         track's left/right edges (fade to bg-surface), shown only when
         neighbours peek. An overlay (not a `mask` on the track) keeps the
         bubbles' `backdrop-filter` halo intact. -->
    {#if edgeFade}
      <div class="edge-fade-overlay edge-fade-overlay-left" aria-hidden="true"></div>
      <div class="edge-fade-overlay edge-fade-overlay-right" aria-hidden="true"></div>
    {/if}
  </div>

  <!-- Playback indicator, one segment per stage: the centred stage is a progress
       bar, the others dots; scrolling morphs between them. Each segment's fill is
       interpolated from the live scroll position (see fillPct): played stages
       (left of centre) read full, the centred stage reads its live progress, and
       upcoming stages read empty, so scrolling slides a segment smoothly between
       those states rather than snapping on settle. -->
  <div class="shrink-0 w-full max-w-xl mx-auto px-4">
    <div class="flex items-center gap-1">
      {#each meta as m, i (i)}
        <button
          type="button"
          onclick={() => scrollToIndex(i)}
          title={m.title}
          aria-label={m.title}
          class="relative h-1.5 rounded-large overflow-hidden hover:cursor-pointer bg-surface-inset"
          style="flex-grow: {weight(i)}; flex-basis: 0.375rem; min-width: 0.375rem"
        >
          <div
            class="absolute inset-y-0 left-0 bg-default-400 rounded-large"
            style="width: {fillPct(i)}%"
          ></div>
        </button>
      {/each}
    </div>
  </div>

  <!-- Reveal scrim: a surface-colored cover painted OVER the whole showcase that
       fades OUT once the layout has settled (driven by the critical `data-js` /
       `main[data-ready]` gate in BaseLayout's <head>). It is a fade, but never
       `opacity` on the section itself: opacity < 1 on a bubble ancestor kills its
       `backdrop-filter` halo in WebKit (same reason the dim/edge fades are
       overlays, not masks). A sibling scrim is not a bubble ancestor, so the
       halos survive. All its styling lives in the critical head CSS so it covers
       on the very first paint, before the UnoCSS utilities load. -->
  <div class="showcase-scrim" aria-hidden="true"></div>
</section>

<style>
  /* The shared chat/settings components cap their widths to the app window via
     `100vw`. In the demo the viewport is the browser, not that fixed window, so
     re-pin the caps to the window width (700px) inside the frame for a 1:1 render.
     Scoped to `.demo-frame` so real bubbles elsewhere on the site keep the live cap. */
  :global(.demo-frame .bubble-body) {
    max-width: calc(700px - 5rem);
  }
  :global(.demo-frame .max-w-\[calc\(100vw-135px\)\]) {
    max-width: calc(700px - 135px);
  }
  :global(.demo-frame .max-w-\[calc\(100vw-80px\)\]) {
    max-width: calc(700px - 80px);
  }
</style>
