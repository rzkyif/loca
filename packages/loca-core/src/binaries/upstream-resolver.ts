// Runtime resolution of upstream sidecar binaries.
//
// On the LATEST channel, binaries.json carries resolver entries (an upstream
// GitHub repo + per-triple asset-name patterns) instead of pinned URLs, so a
// latest core installs upstream releases without us having to re-publish the
// manifest. A resolver without `pinnedTag` tracks the LATEST release; with
// `pinnedTag` it resolves that exact release (deno is pinned this way on
// every channel, because the permission prompt parsing in
// extensions/prompt-parser.ts depends on the prompt wording of the pinned
// version). The signed manifest commits to the repo + patterns; the concrete
// download is verified against GitHub's own published sha256 digest. We
// REFUSE to install an asset that lacks a sha256 digest. There'd be no
// verifiable anchor for the bytes we're about to execute.
//
// A latest resolver does NOT blindly take `/releases/latest`: upstream CI (e.g.
// llama.cpp) publishes the release tag FIRST and then uploads its per-triple
// asset matrix over the following minutes. During that window the newest
// release exists with its assets absent or half-uploaded (`state !== "uploaded"`),
// so resolving against it would fail with no size and never download. Instead we
// scan the most recent releases newest-first and pick the newest one that
// actually carries this triple's asset(s) fully uploaded + digest-verified.
//
// On the STABLE channel, manifest entries are pinned at release time and this
// module isn't involved.

import type { BinaryManifestEntry, BinaryVariant, Triple, UpstreamResolver } from "@loca/shared";
import { assetVariants, errMessage, isResolverEntry, platformVariants } from "@loca/shared";
import { AppError } from "@loca/core-engine";

interface GitHubReleaseAsset {
  name: string;
  browser_download_url: string;
  digest?: string | null;
  size?: number;
  /** GitHub marks an asset "uploaded" only once its bytes are fully stored;
   *  "starter"/"new" mean an in-progress upload we must not resolve against. */
  state?: string;
}

interface GitHubRelease {
  tag_name: string;
  assets: GitHubReleaseAsset[];
  /** Drafts (only visible to authed maintainers) and prereleases are excluded by
   *  `/releases/latest`; the list endpoint includes them, so we filter them out
   *  to keep the same "latest stable full release" semantics. */
  draft?: boolean;
  prerelease?: boolean;
}

// How many recent releases to scan for a complete asset when tracking latest.
// llama.cpp cuts a release roughly hourly, so a handful covers any realistic
// upload-in-progress window without pinning users to a stale build.
const RECENT_RELEASES_PAGE = 10;

/** Concrete download target resolved from an entry for one triple + variant. */
export interface ResolvedBinary {
  version: string;
  /** The GPU-backend variant this download is (`cpu` for single-variant kinds). */
  variant: BinaryVariant;
  url: string;
  sha256: string;
  /** Companion archives (e.g. the Windows CUDA `cudart` runtime) extracted
   *  libs-only into the same bin/lib/<kind> dir. */
  extras?: { url: string; sha256: string }[];
  /** Download size in bytes when the source publishes it (GitHub asset size on
   *  latest resolver entries), summed over the primary + extras. Pinned entries
   *  leave this unset. */
  sizeBytes?: number;
}

// Short-lived per-repo cache of the latest-release lookup. A latest update check
// resolves every sidecar kind, and the UI may poll "check for updates"
// repeatedly; unauthenticated GitHub is rate-limited (~60/hr), so we dedupe
// rapid re-resolves. The signed manifest's resolver config is the trust
// anchor; this only caches the volatile version lookup, briefly.
const RELEASE_CACHE_TTL_MS = 5 * 60_000;
const releaseCache = new Map<string, { releases: GitHubRelease[]; atMs: number }>();

/** Test hook: drop the cached releases so mocked-fetch cases stay isolated. */
export function __resetResolverCacheForTesting(): void {
  releaseCache.clear();
}

async function githubJson(url: string, signal?: AbortSignal): Promise<unknown> {
  const headers: Record<string, string> = {
    accept: "application/vnd.github+json",
    "user-agent": "loca-core",
  };
  const token = Deno.env.get("GITHUB_TOKEN");
  if (token) headers.authorization = `Bearer ${token}`;
  let res: Response;
  try {
    res = await fetch(url, { headers, signal });
  } catch (err) {
    throw new AppError(
      "manifest_fetch_failed",
      `GitHub fetch failed for ${url}: ${errMessage(err)}`,
    );
  }
  if (!res.ok) {
    throw new AppError(
      "manifest_fetch_failed",
      `GitHub API ${res.status} ${res.statusText} for ${url}`,
    );
  }
  return await res.json();
}

/** The candidate releases to resolve against, newest-first. A `pinnedTag`
 *  yields exactly that release; otherwise the most recent page of releases (so
 *  we can skip past a just-published-but-still-uploading newest release). Cached
 *  briefly per repo+tag: an update check resolves every kind and the UI may poll
 *  repeatedly, while unauthenticated GitHub is rate-limited (~60/hr). */
async function candidateReleases(
  repo: string,
  pinnedTag: string | undefined,
  signal?: AbortSignal,
): Promise<GitHubRelease[]> {
  const cacheKey = pinnedTag ? `${repo}@${pinnedTag}` : repo;
  const cached = releaseCache.get(cacheKey);
  if (cached && Date.now() - cached.atMs < RELEASE_CACHE_TTL_MS) {
    return cached.releases;
  }
  let releases: GitHubRelease[];
  if (pinnedTag) {
    const rel = (await githubJson(
      `https://api.github.com/repos/${repo}/releases/tags/${pinnedTag}`,
      signal,
    )) as GitHubRelease;
    releases = [rel];
  } else {
    const list = (await githubJson(
      `https://api.github.com/repos/${repo}/releases?per_page=${RECENT_RELEASES_PAGE}`,
      signal,
    )) as GitHubRelease[];
    // GitHub returns releases newest-first; keep that order. Exclude drafts and
    // prereleases so an un-pinned resolver tracks the latest STABLE release,
    // matching what `/releases/latest` (which the list endpoint does not) does.
    releases = Array.isArray(list) ? list.filter((r) => !r.draft && !r.prerelease) : [];
  }
  releaseCache.set(cacheKey, { releases, atMs: Date.now() });
  return releases;
}

/** Find a named, fully-uploaded, digest-verified asset in a release. Returns
 *  null when the asset is unusable from this release for ANY reason - absent,
 *  still uploading, or lacking a sha256 digest - so the caller falls back to an
 *  older complete release. We never return an undigested asset (an unverifiable
 *  binary), so refusing one is preserved; we just prefer an older verifiable
 *  release over failing outright. */
function resolveAssetInRelease(
  release: GitHubRelease,
  pattern: string,
): { url: string; sha256: string; sizeBytes?: number } | null {
  const assetName = pattern.replace(/\{tag\}/g, release.tag_name);
  const asset = release.assets.find((a) => a.name === assetName);
  if (!asset) return null;
  // An in-progress upload (state "starter"/"new") is not yet resolvable; treat
  // it like an absent asset so we fall back to the last complete release.
  if (asset.state !== undefined && asset.state !== "uploaded") return null;
  // No sha256 digest -> unverifiable -> not installable. Skip to an older
  // release that publishes one rather than failing (GitHub can populate the
  // digest slightly after an upload; an older release is already settled).
  if (!asset.digest || !asset.digest.startsWith("sha256:")) return null;
  return {
    url: asset.browser_download_url,
    sha256: asset.digest.slice("sha256:".length),
    sizeBytes: typeof asset.size === "number" ? asset.size : undefined,
  };
}

/** Resolve one release's primary asset + all its `extra` companions for a
 *  variant, all from the SAME release (so a cuda build and its cudart runtime
 *  match). Returns null when any required piece is absent/uploading in this
 *  release. */
function resolveVariantInRelease(
  release: GitHubRelease,
  variant: BinaryVariant,
  va: { asset: string; extra?: string[] },
): ResolvedBinary | null {
  const primary = resolveAssetInRelease(release, va.asset);
  if (!primary) return null;
  const extras: { url: string; sha256: string; sizeBytes?: number }[] = [];
  for (const p of va.extra ?? []) {
    const e = resolveAssetInRelease(release, p);
    if (!e) return null;
    extras.push(e);
  }
  const sizeBytes = [primary, ...extras].reduce<number | undefined>(
    (sum, a) => (sum === undefined || a.sizeBytes === undefined ? undefined : sum + a.sizeBytes),
    0,
  );
  return {
    version: release.tag_name,
    variant,
    url: primary.url,
    sha256: primary.sha256,
    ...(extras.length ? { extras: extras.map((e) => ({ url: e.url, sha256: e.sha256 })) } : {}),
    sizeBytes,
  };
}

/** Resolve an upstream resolver to a binary for `triple` + `variant`, picking
 *  the newest recent release that actually carries the asset(s) fully uploaded.
 *  Returns null when upstream doesn't publish this triple/variant (the
 *  resolver's `assets` map omits it), so it is marked unavailable rather than
 *  missing. Throws when no recent release has a fully-uploaded, digest-verified
 *  copy of the asset (upstream renamed it, every candidate is still uploading,
 *  or none publishes a digest). */
export async function resolveUpstream(
  resolver: UpstreamResolver,
  triple: Triple,
  variant: BinaryVariant,
  signal?: AbortSignal,
): Promise<ResolvedBinary | null> {
  const va = assetVariants(resolver.assets[triple])[variant];
  if (!va) return null;
  const releases = await candidateReleases(resolver.repo, resolver.pinnedTag, signal);
  for (const release of releases) {
    const resolved = resolveVariantInRelease(release, variant, va);
    if (resolved) return resolved;
  }
  const assetName = va.asset.replace(/\{tag\}/g, releases[0]?.tag_name ?? "{tag}");
  throw new AppError(
    "manifest_fetch_failed",
    `upstream ${resolver.repo} has no fully-uploaded, digest-verified asset ` +
      `"${assetName}" in the ${releases.length} most recent release(s)`,
  );
}

/** Resolve a manifest entry (pinned or resolver) to a concrete download for
 *  `triple` + `variant`. Pinned entries return their stored URL+hash; resolver
 *  entries hit GitHub for the latest release. Returns null when the
 *  triple/variant isn't covered. */
export async function resolveBinaryEntry(
  entry: BinaryManifestEntry,
  triple: Triple,
  variant: BinaryVariant,
  signal?: AbortSignal,
): Promise<ResolvedBinary | null> {
  if (isResolverEntry(entry)) {
    return await resolveUpstream(entry.resolver, triple, variant, signal);
  }
  const target = platformVariants(entry.platforms[triple])[variant];
  if (!target) return null;
  return {
    version: entry.version,
    variant,
    url: target.url,
    sha256: target.sha256,
    ...(target.extras ? { extras: target.extras } : {}),
  };
}
