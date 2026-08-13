<script lang="ts" module>
  // The single markdown renderer for the whole product: the client's chat
  // bubbles and the website's showcase both mount THIS component, so a rendered
  // answer is byte-for-byte the same wherever it appears (single-source rule, see
  // AGENTS.md). Host-specific behaviour (how an external link opens, where errors
  // are logged) is injected via props, so the component stays pure: it imports no
  // client/`$lib`/Tauri code and renders identically in the client, the website,
  // and a future mobile build.
  //
  // marked, highlight.js, marked-highlight, and the hljs theme are deferred to
  // first use so ~600KB of JS stays off the initial bundle until the first
  // markdown message actually renders.
  type MarkedModule = typeof import("marked");
  type Marked = MarkedModule["marked"];

  let rendererReady: Promise<Marked> | null = null;
  // Set once the loader resolves, so a warm component renders synchronously
  // (no spinner, no extra microtask) instead of awaiting the cached promise.
  let markedNow: Marked | null = null;

  /** Load (once) and configure the marked instance. Exposed so a host that needs
   *  a synchronous first paint (e.g. a showcase that measures its own height
   *  before animating) can warm the loader up front; afterwards `content`
   *  changes render synchronously. */
  export function preloadMarkdown(): Promise<Marked> {
    if (rendererReady) return rendererReady;
    rendererReady = (async () => {
      const [hljsMod, markedMod, markedHighlightMod] = await Promise.all([
        import("highlight.js"),
        import("marked"),
        import("marked-highlight"),
      ]);
      // The hljs theme stylesheet is purely cosmetic (code-token colours), so it
      // loads best-effort and OUTSIDE the Promise.all above: a host that chunks
      // but never emits/serves this asset (the Astro website does exactly this)
      // would otherwise reject the whole renderer load and freeze anything that
      // awaits it (e.g. the homepage showcase warming up before it animates).
      import("highlight.js/styles/atom-one-dark.css").catch(() => {});
      const hljs = hljsMod.default;
      const { marked } = markedMod;
      const { markedHighlight } = markedHighlightMod;
      marked.use(
        markedHighlight({
          langPrefix: "hljs language-",
          highlight(code, lang) {
            const language = hljs.getLanguage(lang) ? lang : "plaintext";
            return hljs.highlight(code, { language }).value;
          },
        }),
      );
      markedNow = marked;
      return marked;
    })();
    return rendererReady;
  }

  const ALLOWED_TAGS = [
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "strong",
    "em",
    "del",
    "ul",
    "ol",
    "li",
    "code",
    "pre",
    "a",
    "img",
    "blockquote",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "hr",
    "br",
    "div",
    "span",
    "input",
    "mark",
    "kbd",
    "sub",
    "sup",
    "details",
    "summary",
  ];

  const ALLOWED_ATTR = [
    "href",
    "src",
    "alt",
    "class",
    "type",
    "checked",
    "disabled",
    "start",
    "value",
    "reversed",
  ];
</script>

<script lang="ts">
  import DOMPurify from "dompurify";
  import { tick } from "svelte";

  let {
    content,
    isStreaming = false,
    onOpenLink = (href: string) => {
      globalThis.open?.(href, "_blank", "noopener,noreferrer");
    },
    onError = (message: string, err: unknown) => console.error(message, err),
  }: {
    content: string;
    isStreaming?: boolean;
    /** Open a clicked http(s) link. The client passes the OS-browser opener; the
     *  default opens a new tab. */
    onOpenLink?: (href: string) => void;
    /** Report a renderer-load failure. Defaults to `console.error`. */
    onError?: (message: string, err: unknown) => void;
  } = $props();

  // Intercept clicks on links in rendered (untrusted, model-authored) markdown so
  // the host decides how they open, rather than letting them navigate in-frame (a
  // prompt-injected link could otherwise take over the app chrome). DOMPurify has
  // already stripped javascript: hrefs.
  function handleLinkClick(e: MouseEvent) {
    const anchor = (e.target as HTMLElement | null)?.closest("a");
    const href = anchor?.getAttribute("href");
    if (!href || !/^https?:\/\//i.test(href)) return;
    e.preventDefault();
    onOpenLink(href);
  }

  // svelte-ignore non_reactive_update
  // oxlint-disable-next-line no-unassigned-vars
  let container: HTMLDivElement;

  let renderedHtml = $state<string | null>(null);
  // Attach the external-link interceptor once via delegation on the container
  // (rather than an inline handler on a static element, which trips a11y lints).
  let linkHandlerAttached = false;

  // Streaming flushes fire ~33x/s. Parsing + sanitizing + re-highlighting the
  // full (growing) message on every one is O(n^2) over the answer length and pins
  // a CPU core on long replies. While streaming we coalesce to at most one parse
  // per STREAM_PARSE_THROTTLE_MS (still feels live) and always run a final parse
  // when streaming ends so the completed message renders cleanly.
  const STREAM_PARSE_THROTTLE_MS = 120;
  let lastParseAt = 0;
  let throttleTimer: ReturnType<typeof setTimeout> | undefined;
  // Bumped per parse so a superseded async parse can't overwrite a newer one.
  let parseGen = 0;

  function wrapTables(node: HTMLDivElement) {
    const tables = node.querySelectorAll("table");
    tables.forEach((table) => {
      if (!table.parentElement?.classList.contains("table-scroller")) {
        const wrapper = document.createElement("div");
        wrapper.className = "table-wrapper";
        const scroller = document.createElement("div");
        scroller.className = "table-scroller loca-scroll";
        table.parentNode?.insertBefore(wrapper, table);
        wrapper.appendChild(scroller);
        scroller.appendChild(table);
      }
    });
  }

  function wrapCodeBlocks(node: HTMLDivElement) {
    const pres = node.querySelectorAll("pre");
    pres.forEach((pre) => {
      if (!pre.parentElement?.classList.contains("code-scroller")) {
        const wrapper = document.createElement("div");
        wrapper.className = "code-wrapper";
        const scroller = document.createElement("div");
        scroller.className = "code-scroller loca-scroll-dark";
        pre.parentNode?.insertBefore(wrapper, pre);
        wrapper.appendChild(scroller);
        scroller.appendChild(pre);
      }
    });
  }

  // Parse + sanitize with the (already-loaded) renderer, then wrap tables/code
  // blocks once the DOM is live. `gen` guards against a superseded parse.
  function applyParse(text: string, gen: number) {
    if (gen !== parseGen || !markedNow) return;
    renderedHtml = DOMPurify.sanitize(markedNow.parse(text) as string, {
      ALLOWED_TAGS,
      ALLOWED_ATTR,
      ALLOW_DATA_ATTR: false,
    });
    tick().then(() => {
      if (!container) return;
      if (!linkHandlerAttached) {
        container.addEventListener("click", handleLinkClick);
        linkHandlerAttached = true;
      }
      wrapTables(container);
      wrapCodeBlocks(container);
    });
  }

  function renderNow(text: string) {
    const gen = ++parseGen;
    lastParseAt = Date.now();
    // Warm renderer: render synchronously. Cold: lazy-load it (the spinner shows
    // until it resolves), then parse.
    if (markedNow) {
      applyParse(text, gen);
      return;
    }
    preloadMarkdown()
      .then(() => applyParse(text, gen))
      .catch((err) => {
        if (gen !== parseGen) return;
        // The lazily-loaded renderer (marked / highlight.js) failed to import.
        // Don't leave a permanent spinner: fall back to escaped plain text, and
        // reset the cached loader so a later message retries.
        onError("markdown renderer failed to load; showing plain text", err);
        rendererReady = null;
        renderedHtml = text.replace(
          /[&<>"']/g,
          (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
        );
      });
  }

  $effect(() => {
    const text = content;
    const streaming = isStreaming;
    if (!text) {
      renderedHtml = null;
      return;
    }
    if (!streaming) {
      // Final or non-streamed render: parse the complete text immediately and
      // drop any pending throttled parse.
      if (throttleTimer) {
        clearTimeout(throttleTimer);
        throttleTimer = undefined;
      }
      renderNow(text);
      return;
    }
    // Streaming: coalesce to a coarse cadence. A trailing timer always reads the
    // freshest `content`, so no streamed chunk is lost.
    if (throttleTimer) return;
    const elapsed = Date.now() - lastParseAt;
    if (elapsed >= STREAM_PARSE_THROTTLE_MS) {
      renderNow(text);
    } else {
      throttleTimer = setTimeout(() => {
        throttleTimer = undefined;
        renderNow(content);
      }, STREAM_PARSE_THROTTLE_MS - elapsed);
    }
  });

  $effect(() => {
    return () => {
      if (throttleTimer) clearTimeout(throttleTimer);
    };
  });
</script>

{#if content}
  {#if renderedHtml === null}
    <i class="i-line-md:loading-loop text-3xl"></i>
  {:else}
    <div bind:this={container} class="markdown-content min-w-0 overflow-hidden">
      {@html renderedHtml}
    </div>
  {/if}
{/if}
