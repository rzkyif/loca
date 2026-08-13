// Mirror a release to GitHub Releases, alongside the R2 distribution.
//
// One ROLLING release per channel: tag `latest` or `stable`, moved to this run's
// commit and re-populated each time (assets clobbered, body regenerated). This is
// an additive mirror of what was just uploaded to R2; R2 stays the canonical
// source the app fetches from. Driven by the `gh` CLI (preinstalled on GitHub
// runners) authenticated via GITHUB_TOKEN with `contents: write`.
//
// Called from runReleasePlan (lib.ts) after the R2 upload but BEFORE the cursor
// write, so a mirror failure fails the whole run and a re-push retries idempotently.

import { join } from "@std/path";
import { walk } from "@std/fs/walk";
import {
  channelManifestDir,
  colors,
  type DeployEnv,
  DIST_DIR,
  exists,
  fail,
  info,
  ok,
  type ReleaseChannel,
  rel,
  step,
} from "./lib.ts";

export interface GithubReleaseInput {
  /** The changed items + the versions just published, for the release body. */
  items: Array<{ label: string; version: string }>;
  /** Local files to attach, with the flat asset name to display them under. */
  assets: Array<{ path: string; name: string }>;
}

/** Run `gh`, inheriting the process env (so it sees GITHUB_TOKEN / GH_REPO). */
async function gh(args: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  const cmd = new Deno.Command("gh", { args, stdout: "piped", stderr: "piped" });
  const out = await cmd
    .output()
    .catch(() =>
      fail(`could not run \`gh\`; the GitHub CLI must be installed for --github-release`),
    );
  return {
    code: out.code,
    stdout: new TextDecoder().decode(out.stdout),
    stderr: new TextDecoder().decode(out.stderr),
  };
}

/** Commit the rolling tag should point at: the CI-provided SHA, else HEAD. */
async function targetSha(): Promise<string> {
  const fromCi = Deno.env.get("GITHUB_SHA");
  if (fromCi) return fromCi;
  const out = await new Deno.Command("git", {
    args: ["rev-parse", "HEAD"],
    stdout: "piped",
    stderr: "null",
  }).output();
  if (!out.success) fail(`could not resolve HEAD for the GitHub Release target`);
  return new TextDecoder().decode(out.stdout).trim();
}

/** Every signed manifest + sidecar this run wrote, attached so the release is a
 *  self-contained mirror of the channel's manifest set. */
async function manifestAssets(
  channel: ReleaseChannel,
): Promise<Array<{ path: string; name: string }>> {
  const dir = join(DIST_DIR, channelManifestDir(channel));
  if (!(await exists(dir))) return [];
  const out: Array<{ path: string; name: string }> = [];
  for await (const entry of walk(dir, { includeDirs: false, exts: [".json", ".sig"] })) {
    out.push({ path: entry.path, name: entry.name });
  }
  return out;
}

function releaseBody(channel: ReleaseChannel, items: GithubReleaseInput["items"]): string {
  const lines = [
    `Automated **${channel}** channel release, mirrored from the R2 distribution (\`get.au.tomat.ing\`).\n`,
    `Published items:`,
    "",
    ...items.map((i) => `- ${i.label} v${i.version}`),
    "",
    `Follow the installation instructions at https://au.tomat.ing/install/`,
  ];
  return lines.join("\n");
}

export async function publishGithubRelease(
  _env: DeployEnv,
  channel: ReleaseChannel,
  input: GithubReleaseInput,
): Promise<void> {
  step(`Mirroring to the rolling "${channel}" GitHub Release`);

  const tag = channel; // "latest" | "stable"
  const title = `Tomat ${channel}`;
  const sha = await targetSha();
  const body = releaseBody(channel, input.items);

  // Dedupe assets by display name (last wins); manifests + item-recorded files.
  const byName = new Map<string, string>();
  for (const a of [...(await manifestAssets(channel)), ...input.assets]) {
    byName.set(a.name, a.path);
  }
  const assets = [...byName].map(([name, path]) => ({ name, path }));

  const repoArgs = Deno.env.get("GITHUB_REPOSITORY")
    ? ["--repo", Deno.env.get("GITHUB_REPOSITORY")!]
    : [];

  // Upsert the release and move its tag to this commit. `gh release view` exits
  // non-zero when the release does not exist yet.
  const view = await gh(["release", "view", tag, ...repoArgs]);
  // The latest channel is a prerelease (newest, not yet promoted); stable is the
  // GitHub "Latest release".
  const stabilityFlag = channel === "stable" ? ["--latest"] : ["--prerelease"];
  if (view.code === 0) {
    const edit = await gh([
      "release",
      "edit",
      tag,
      ...repoArgs,
      "--target",
      sha,
      "--title",
      title,
      "--notes",
      body,
      ...stabilityFlag,
    ]);
    if (edit.code !== 0) fail(`gh release edit ${tag} failed: ${edit.stderr.trim()}`);
    // `gh release edit --target` does NOT move an already-created git tag, so move
    // the rolling tag to this commit via the refs API (best-effort: a fresh tag is
    // already at sha). This is a tag ref, not a branch commit.
    const repoPath = Deno.env.get("GITHUB_REPOSITORY") ?? "{owner}/{repo}";
    const move = await gh([
      "api",
      "-X",
      "PATCH",
      `repos/${repoPath}/git/refs/tags/${tag}`,
      "-f",
      `sha=${sha}`,
      "-F",
      "force=true",
    ]);
    if (move.code !== 0) info(colors.yellow(`could not move tag ${tag}: ${move.stderr.trim()}`));
    ok(`updated GitHub Release ${tag} -> ${sha.slice(0, 12)}`);
  } else {
    const create = await gh([
      "release",
      "create",
      tag,
      ...repoArgs,
      "--target",
      sha,
      "--title",
      title,
      "--notes",
      body,
      ...stabilityFlag,
    ]);
    if (create.code !== 0) fail(`gh release create ${tag} failed: ${create.stderr.trim()}`);
    ok(`created GitHub Release ${tag} @ ${sha.slice(0, 12)}`);
  }

  if (assets.length === 0) {
    info(colors.yellow(`no assets to attach to the ${tag} release`));
    return;
  }

  // GitHub keys a release asset by its FILENAME, and `path#label` only sets a
  // display label - the stored name stays the file's basename. Several artifacts
  // share a basename across triples (tomat-core-latest.gz, tomat-latest.app.tar.gz,
  // ...), so uploading them directly makes gh clobber same-named files in parallel
  // and 404. Stage each under its already-unique flat name (a.name) so every
  // uploaded filename is distinct.
  info(`attaching ${assets.length} asset(s) to ${tag}`);
  const uploadDir = await Deno.makeTempDir({ prefix: "tomat-ghrel-" });
  const uploadFiles: string[] = [];
  for (const a of assets) {
    const staged = join(uploadDir, a.name);
    await Deno.copyFile(a.path, staged);
    uploadFiles.push(staged);
  }
  // --clobber replaces same-named assets so the rolling release is overwritten in
  // place. A just-created/edited release can still 404 on uploads.github.com for a
  // few seconds until it replicates from api.github.com; retry with backoff.
  const uploadArgs = ["release", "upload", tag, ...repoArgs, ...uploadFiles, "--clobber"];
  let upload = await gh(uploadArgs);
  for (let attempt = 1; upload.code !== 0 && attempt <= 5; attempt++) {
    info(colors.yellow(`upload failed (attempt ${attempt}/5); retrying in ${attempt * 3}s`));
    await new Promise((resolve) => setTimeout(resolve, attempt * 3000));
    upload = await gh(uploadArgs);
  }
  await Deno.remove(uploadDir, { recursive: true }).catch(() => {});
  if (upload.code !== 0) fail(`gh release upload ${tag} failed: ${upload.stderr.trim()}`);
  ok(`attached ${assets.length} asset(s) to ${tag} (${rel(DIST_DIR)} mirror)`);
}
