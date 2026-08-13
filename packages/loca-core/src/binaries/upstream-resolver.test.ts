import { assertEquals, assertRejects } from "@std/assert";
import { isResolverEntry } from "@loca/shared";
import type { BinaryManifestEntry } from "@loca/shared";
import {
  __resetResolverCacheForTesting,
  resolveBinaryEntry,
  resolveUpstream,
} from "./upstream-resolver.ts";

// Swap globalThis.fetch for a canned GitHub response so the resolution branches
// are exercised without touching the network. A latest (un-pinned) resolver
// hits `/releases?per_page=N` (a newest-first ARRAY); pass a bare release object
// and it is wrapped as a one-element list.
function withMockReleases(body: unknown, fn: () => Promise<unknown>): () => Promise<void> {
  const list = Array.isArray(body) ? body : [body];
  return async () => {
    const orig = globalThis.fetch;
    __resetResolverCacheForTesting();
    globalThis.fetch = (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      // Pinned-tag lookups hit `/releases/tags/<tag>` (a single object); the
      // latest path hits `/releases?per_page=N` (an array).
      const payload = url.includes("/releases/tags/") ? list[0] : list;
      return Promise.resolve(
        new Response(JSON.stringify(payload), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    };
    try {
      await fn();
    } finally {
      globalThis.fetch = orig;
      __resetResolverCacheForTesting();
    }
  };
}

Deno.test("isResolverEntry distinguishes pinned vs resolver entries", () => {
  const pinned: BinaryManifestEntry = {
    version: "v1",
    platforms: {} as never,
  };
  const resolver: BinaryManifestEntry = {
    resolver: { repo: "a/b", assets: {} },
  };
  assertEquals(isResolverEntry(pinned), false);
  assertEquals(isResolverEntry(resolver), true);
});

Deno.test("resolveBinaryEntry returns the pinned URL+hash without network", async () => {
  const entry: BinaryManifestEntry = {
    version: "b9310",
    platforms: {
      // Bare pinned target normalizes to the cpu variant.
      "aarch64-apple-darwin": { url: "https://example/x", sha256: "abc" },
    } as never,
  };
  const got = await resolveBinaryEntry(entry, "aarch64-apple-darwin", "cpu");
  assertEquals(got, {
    version: "b9310",
    variant: "cpu",
    url: "https://example/x",
    sha256: "abc",
  });
});

Deno.test("resolveBinaryEntry resolves a pinned GPU variant with its extras", async () => {
  const entry: BinaryManifestEntry = {
    version: "b9310",
    platforms: {
      "x86_64-pc-windows-msvc": {
        cpu: { url: "https://example/cpu", sha256: "c" },
        cuda: {
          url: "https://example/cuda",
          sha256: "g",
          extras: [{ url: "https://example/cudart", sha256: "r" }],
        },
      },
    } as never,
  };
  assertEquals(await resolveBinaryEntry(entry, "x86_64-pc-windows-msvc", "cuda"), {
    version: "b9310",
    variant: "cuda",
    url: "https://example/cuda",
    sha256: "g",
    extras: [{ url: "https://example/cudart", sha256: "r" }],
  });
});

Deno.test("resolveBinaryEntry returns null for a triple the entry doesn't cover", async () => {
  const entry: BinaryManifestEntry = {
    version: "b9310",
    platforms: {
      "aarch64-apple-darwin": { url: "https://example/x", sha256: "abc" },
    } as never,
  };
  assertEquals(await resolveBinaryEntry(entry, "x86_64-pc-windows-msvc", "cpu"), null);
});

Deno.test("resolveBinaryEntry returns null for a variant the triple doesn't offer", async () => {
  const entry: BinaryManifestEntry = {
    version: "b9310",
    platforms: {
      "aarch64-apple-darwin": { url: "https://example/x", sha256: "abc" },
    } as never,
  };
  // mac ships a single (cpu) build; asking for cuda yields null.
  assertEquals(await resolveBinaryEntry(entry, "aarch64-apple-darwin", "cuda"), null);
});

Deno.test("resolveUpstream returns null (no GitHub call) when the triple has no asset", async () => {
  // whisper.cpp ships only Windows assets, so mac/linux latest must degrade to
  // null, not throw, and must not hit the network.
  const resolver = {
    repo: "ggml-org/whisper.cpp",
    assets: { "x86_64-pc-windows-msvc": "whisper-bin-x64.zip" },
  };
  assertEquals(await resolveUpstream(resolver, "aarch64-apple-darwin", "cpu"), null);
});

Deno.test(
  "resolveUpstream resolves the latest asset + verifies via GitHub's sha256 digest",
  withMockReleases(
    {
      tag_name: "b9999",
      assets: [
        {
          name: "llama-b9999-bin-macos-arm64.tar.gz",
          browser_download_url: "https://github.test/llama-b9999.tar.gz",
          digest: "sha256:deadbeef",
          size: 8675309,
        },
      ],
    },
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: {
            "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz",
          },
        },
        "aarch64-apple-darwin",
        "cpu",
      );
      assertEquals(got, {
        version: "b9999",
        variant: "cpu",
        url: "https://github.test/llama-b9999.tar.gz",
        sha256: "deadbeef",
        sizeBytes: 8675309,
      });
    },
  ),
);

Deno.test(
  "resolveUpstream resolves a GPU variant asset + its cudart extra",
  withMockReleases(
    {
      tag_name: "b9999",
      assets: [
        {
          name: "llama-b9999-bin-win-cuda-13.3-x64.zip",
          browser_download_url: "https://github.test/cuda.zip",
          digest: "sha256:aaa",
          size: 100,
        },
        {
          name: "cudart-llama-bin-win-cuda-13.3-x64.zip",
          browser_download_url: "https://github.test/cudart.zip",
          digest: "sha256:bbb",
          size: 50,
        },
      ],
    },
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: {
            "x86_64-pc-windows-msvc": {
              cpu: "llama-{tag}-bin-win-cpu-x64.zip",
              cuda: {
                asset: "llama-{tag}-bin-win-cuda-13.3-x64.zip",
                extra: ["cudart-llama-bin-win-cuda-13.3-x64.zip"],
              },
            },
          },
        },
        "x86_64-pc-windows-msvc",
        "cuda",
      );
      assertEquals(got, {
        version: "b9999",
        variant: "cuda",
        url: "https://github.test/cuda.zip",
        sha256: "aaa",
        extras: [{ url: "https://github.test/cudart.zip", sha256: "bbb" }],
        sizeBytes: 150,
      });
    },
  ),
);

Deno.test(
  "resolveUpstream refuses an asset with no sha256 digest (unverifiable), never installs it",
  // Only release, digestless asset: we must NOT return it (unverifiable) and,
  // with no older release to fall back to, reject.
  withMockReleases(
    {
      tag_name: "b9999",
      assets: [
        {
          name: "llama-b9999-bin-macos-arm64.tar.gz",
          browser_download_url: "https://github.test/llama-b9999.tar.gz",
          digest: null,
        },
      ],
    },
    () =>
      assertRejects(
        () =>
          resolveUpstream(
            {
              repo: "ggml-org/llama.cpp",
              assets: {
                "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz",
              },
            },
            "aarch64-apple-darwin",
            "cpu",
          ),
        Error,
        "digest-verified",
      ),
  ),
);

Deno.test(
  "resolveUpstream skips a digestless newest asset and falls back to an older digested one",
  // GitHub can publish a release asset before its digest is computed; rather than
  // fail, fall back to the last release that has a digest-verified copy.
  withMockReleases(
    [
      {
        tag_name: "b9999",
        assets: [
          {
            name: "llama-b9999-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/nodigest.tar.gz",
            digest: null,
            state: "uploaded",
          },
        ],
      },
      {
        tag_name: "b9998",
        assets: [
          {
            name: "llama-b9998-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/mac-b9998.tar.gz",
            digest: "sha256:mac",
            size: 42,
            state: "uploaded",
          },
        ],
      },
    ],
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: { "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz" },
        },
        "aarch64-apple-darwin",
        "cpu",
      );
      assertEquals(got?.version, "b9998");
      assertEquals(got?.sha256, "mac");
    },
  ),
);

Deno.test(
  "resolveUpstream throws when the asset is absent from every recent release",
  withMockReleases(
    [
      { tag_name: "b9999", assets: [{ name: "something-else.zip", browser_download_url: "x" }] },
      { tag_name: "b9998", assets: [{ name: "another.zip", browser_download_url: "y" }] },
    ],
    () =>
      assertRejects(
        () =>
          resolveUpstream(
            {
              repo: "ggml-org/llama.cpp",
              assets: {
                "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz",
              },
            },
            "aarch64-apple-darwin",
            "cpu",
          ),
        Error,
        "no fully-uploaded, digest-verified asset",
      ),
  ),
);

Deno.test(
  "resolveUpstream skips a just-published release still uploading its assets",
  // The newest release (b9999) exists but its macos asset hasn't finished
  // uploading (absent here); resolution must fall back to the last COMPLETE
  // release (b9998) instead of failing. This is the exact llama.cpp
  // publish-before-upload window that stranded llama-server with no size.
  withMockReleases(
    [
      {
        tag_name: "b9999",
        assets: [
          // A different triple's asset finished first; ours isn't here yet.
          {
            name: "llama-b9999-bin-win-cpu-x64.zip",
            browser_download_url: "https://github.test/win.zip",
            digest: "sha256:win",
            size: 1,
          },
        ],
      },
      {
        tag_name: "b9998",
        assets: [
          {
            name: "llama-b9998-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/mac-b9998.tar.gz",
            digest: "sha256:mac",
            size: 42,
            state: "uploaded",
          },
        ],
      },
    ],
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: { "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz" },
        },
        "aarch64-apple-darwin",
        "cpu",
      );
      assertEquals(got, {
        version: "b9998",
        variant: "cpu",
        url: "https://github.test/mac-b9998.tar.gz",
        sha256: "mac",
        sizeBytes: 42,
      });
    },
  ),
);

Deno.test(
  "resolveUpstream skips a prerelease and resolves the newest stable release",
  // `/releases` (unlike `/releases/latest`) lists prereleases/drafts; an
  // un-pinned resolver must track the latest STABLE release, not a prerelease.
  withMockReleases(
    [
      {
        tag_name: "b9999-rc",
        prerelease: true,
        assets: [
          {
            name: "llama-b9999-rc-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/rc.tar.gz",
            digest: "sha256:rc",
            size: 9,
            state: "uploaded",
          },
        ],
      },
      {
        tag_name: "b9998",
        assets: [
          {
            name: "llama-b9998-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/mac-b9998.tar.gz",
            digest: "sha256:mac",
            size: 42,
            state: "uploaded",
          },
        ],
      },
    ],
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: { "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz" },
        },
        "aarch64-apple-darwin",
        "cpu",
      );
      assertEquals(got?.version, "b9998");
      assertEquals(got?.sha256, "mac");
    },
  ),
);

Deno.test(
  "resolveUpstream skips an asset whose upload is still in progress (state != uploaded)",
  // The newest release lists our asset but GitHub still marks it "starter"
  // (bytes not fully stored); it must be treated as not-yet-available.
  withMockReleases(
    [
      {
        tag_name: "b9999",
        assets: [
          {
            name: "llama-b9999-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/partial.tar.gz",
            digest: "sha256:partial",
            size: 7,
            state: "starter",
          },
        ],
      },
      {
        tag_name: "b9998",
        assets: [
          {
            name: "llama-b9998-bin-macos-arm64.tar.gz",
            browser_download_url: "https://github.test/mac-b9998.tar.gz",
            digest: "sha256:mac",
            size: 42,
            state: "uploaded",
          },
        ],
      },
    ],
    async () => {
      const got = await resolveUpstream(
        {
          repo: "ggml-org/llama.cpp",
          assets: { "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz" },
        },
        "aarch64-apple-darwin",
        "cpu",
      );
      assertEquals(got?.version, "b9998");
      assertEquals(got?.sha256, "mac");
    },
  ),
);
