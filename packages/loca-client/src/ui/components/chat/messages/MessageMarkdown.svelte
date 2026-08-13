<script lang="ts">
  // Thin client wrapper over the shared markdown renderer: it supplies the two
  // host-specific behaviours the pure component can't own (open links in the OS
  // browser; log through the client logger). The parsing, sanitizing,
  // highlighting, table/code wrapping, and stream throttling all live in the
  // shared component, so chat bubbles render byte-for-byte the same markdown as
  // the website showcase.
  import Markdown from "@loca/shared/ui/components/primitives/Markdown.svelte";
  import { platform } from "$lib/platform";
  import { getLogger } from "$lib/util/log";

  const log = getLogger("markdown");

  let { content, isStreaming = false }: { content: string; isStreaming?: boolean } = $props();

  // Open links from rendered (untrusted, model-authored) markdown in the system
  // browser instead of letting them navigate the app webview in-frame. Without
  // this, a prompt-injected link could replace the trusted app chrome
  // (window-takeover / phishing). DOMPurify already strips javascript: hrefs.
  function openExternal(href: string): void {
    void platform()
      .openExternal(href)
      .catch((e) => log.warn("openExternal failed", e));
  }
</script>

<Markdown {content} {isStreaming} onOpenLink={openExternal} onError={(m, e) => log.error(m, e)} />
