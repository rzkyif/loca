// npm registry helpers: search for packages with the `loca-extension`
// keyword and resolve a package@version → tarball URL + sha (for installer).

import type { ExtensionSearchResult } from "@loca/shared";
import { AppError } from "@loca/core-engine";

const REGISTRY_BASE = "https://registry.npmjs.org";
const SEARCH_KEYWORD = "loca-extension";

export interface NpmPackageVersion {
  version: string;
  tarballUrl: string;
  // npm's `dist.shasum` is sha1 (legacy, weak). `dist.integrity` is a
  // Subresource-Integrity string (e.g. "sha512-<base64>") and is the strong
  // hash we verify the downloaded tarball against before extraction.
  shasum?: string;
  integrity?: string;
  description?: string;
  homepage?: string;
  license?: string;
}

export async function searchPackages(
  query: string,
  limit = 50,
  offset = 0,
): Promise<ExtensionSearchResult[]> {
  const params = new URLSearchParams({
    text: `keywords:${SEARCH_KEYWORD} ${query}`.trim(),
    size: String(limit),
    from: String(offset),
  });
  const url = `${REGISTRY_BASE}/-/v1/search?${params}`;
  const res = await fetch(url);
  if (!res.ok) {
    throw new AppError("manifest_fetch_failed", `npm search HTTP ${res.status} ${res.statusText}`);
  }
  const body = (await res.json()) as { objects?: Array<NpmSearchEntry> };
  return (body.objects ?? []).map((o) => ({
    name: o.package.name,
    description: o.package.description ?? "",
    version: o.package.version,
    weeklyDownloads: o.downloads?.weekly,
    source: "npm" as const,
    homepage: o.package.links?.homepage,
    license: o.package.license,
  }));
}

interface NpmSearchEntry {
  package: {
    name: string;
    version: string;
    description?: string;
    license?: string;
    links?: { homepage?: string };
  };
  downloads?: { weekly?: number };
}

/** Lightweight latest-version lookup for update checks: fetches the registry
 *  metadata and returns `dist-tags.latest` without resolving tarball URLs or
 *  integrity (which `resolveVersion` does for an actual install). */
export async function resolveLatestVersion(name: string): Promise<string> {
  const res = await fetch(`${REGISTRY_BASE}/${encodeURIComponent(name)}`);
  if (!res.ok) {
    throw new AppError("manifest_fetch_failed", `npm metadata HTTP ${res.status} for ${name}`);
  }
  const meta = (await res.json()) as { "dist-tags"?: Record<string, string> };
  const latest = meta["dist-tags"]?.latest;
  if (!latest) {
    throw new AppError("extension_not_found", `no latest version for ${name}`);
  }
  return latest;
}

export async function resolveVersion(
  name: string,
  versionSpec?: string,
): Promise<NpmPackageVersion> {
  const res = await fetch(`${REGISTRY_BASE}/${encodeURIComponent(name)}`);
  if (!res.ok) {
    throw new AppError("manifest_fetch_failed", `npm metadata HTTP ${res.status} for ${name}`);
  }
  const meta = (await res.json()) as {
    "dist-tags"?: Record<string, string>;
    versions?: Record<
      string,
      {
        version: string;
        dist: { tarball: string; shasum?: string; integrity?: string };
        description?: string;
        homepage?: string;
        license?: string;
      }
    >;
  };
  const tag =
    versionSpec && versionSpec.startsWith("^")
      ? meta["dist-tags"]?.latest
      : (versionSpec ?? meta["dist-tags"]?.latest);
  const version = tag ?? meta["dist-tags"]?.latest;
  if (!version) {
    throw new AppError("extension_not_found", `no version for ${name}`);
  }
  const entry = meta.versions?.[version];
  if (!entry) {
    throw new AppError("extension_not_found", `no metadata for ${name}@${version}`);
  }
  return {
    version,
    tarballUrl: entry.dist.tarball,
    shasum: entry.dist.shasum,
    integrity: entry.dist.integrity,
    description: entry.description,
    homepage: entry.homepage,
    license: entry.license,
  };
}
