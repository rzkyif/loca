// Git branch alignment for the release pipeline.
//
// The channel branches mirror the R2 channels: `latest` tracks what the latest
// channel has published, `stable` tracks stable. The promotion chain is a clean
// fast-forward: latest <- main, stable <- latest.
//
//   - remote.ts (`deno task remote-release`) fast-forwards one remote branch
//     onto another via the GitHub refs API, independent of the local working
//     tree. Moving the branch is what triggers the CI release workflow.
//   - ci-bump.ts commits the post-release version bumps onto `main` from the
//     CI runner (commitVersionBump + pushMain).
//
// Both no-op when the target is already aligned.

import { fail, info, ok, type PublishedItem, type ReleaseChannel, rel, step } from "./lib.ts";

// ---------------------------------------------------------------------------
// local helpers (the CI bump runner's working tree)

async function git(args: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  const out = await new Deno.Command("git", { args, stdout: "piped", stderr: "piped" })
    .output()
    .catch(() => fail(`could not run \`git\`; is it installed and on PATH?`));
  return {
    code: out.code,
    stdout: new TextDecoder().decode(out.stdout).trim(),
    stderr: new TextDecoder().decode(out.stderr).trim(),
  };
}

export async function revParse(ref: string): Promise<string> {
  const out = await git(["rev-parse", ref]);
  if (out.code !== 0) fail(`git rev-parse ${ref} failed: ${out.stderr}`);
  return out.stdout;
}

/** Commit the just-bumped version files as `bump: <labels>` on the current
 *  HEAD. The bumped items name both the files to stage (deduped, since client +
 *  android share tauri.conf.json) and the labels for the message. Stages only
 *  those exact files (never `-a`), so an unrelated dirty file cannot ride along;
 *  the non-force push that follows guards against a colliding concurrent
 *  change. Returns the new HEAD
 *  sha, or null when nothing was bumped. */
export async function commitVersionBump(bumped: PublishedItem[]): Promise<string | null> {
  if (bumped.length === 0) return null;
  const files = [...new Set(bumped.map((b) => b.versionFile))];
  const labels = [...new Set(bumped.map((b) => b.label))];

  step(`Committing version bump: ${labels.join(", ")}`);
  const add = await git(["add", "--", ...files]);
  if (add.code !== 0) fail(`git add of bumped version files failed: ${add.stderr}`);

  // Stage nothing unexpected: the staged set must equal exactly the bumped files.
  const staged = await git(["diff", "--cached", "--name-only"]);
  if (staged.code !== 0) fail(`git diff --cached failed: ${staged.stderr}`);
  const stagedSet = new Set(staged.stdout.split("\n").filter(Boolean));
  const wantSet = new Set(files.map((f) => rel(f)));
  const unexpected = [...stagedSet].filter((f) => !wantSet.has(f));
  if (unexpected.length > 0) {
    fail(`unexpected staged changes alongside the version bump: ${unexpected.join(", ")}`);
  }

  const commit = await git(["commit", "-m", `bump: ${labels.join(", ")}`]);
  if (commit.code !== 0) fail(`git commit of the version bump failed: ${commit.stderr}`);
  const head = await revParse("HEAD");
  ok(`committed bump ${head.slice(0, 12)} (${files.length} file(s))`);
  return head;
}

/** Fast-forward origin/main to HEAD and push it. No-op when origin/main already
 *  points at HEAD. Non-force, so a diverged main (a concurrent push during the
 *  release) is refused rather than clobbered. Used after a latest-channel
 *  release commits the post-release version bump. */
export async function pushMain(): Promise<void> {
  const head = await revParse("HEAD");
  const remote = await git(["rev-parse", "origin/main"]);
  if (remote.code === 0 && remote.stdout === head) {
    info(`origin/main already at HEAD (${head.slice(0, 12)}); nothing to push`);
    return;
  }
  step(`Fast-forwarding origin/main -> ${head.slice(0, 12)}`);
  const push = await git(["push", "origin", "HEAD:refs/heads/main"]);
  if (push.code !== 0) {
    fail(`git push to main failed (non-fast-forward or rejected):\n${push.stderr}`);
  }
  ok(`pushed HEAD to origin/main`);
}

// ---------------------------------------------------------------------------
// remote helpers (deno task remote-release)
//
// Purely remote: these never read the local working tree, HEAD, or branch
// position. They only resolve the GitHub repo identity ({owner}/{repo}, from the
// origin remote or GITHUB_REPOSITORY) and move refs server-side.

function repoPath(): string {
  return Deno.env.get("GITHUB_REPOSITORY") ?? "{owner}/{repo}";
}

async function gh(args: string[]): Promise<{ code: number; stdout: string; stderr: string }> {
  const out = await new Deno.Command("gh", { args, stdout: "piped", stderr: "piped" })
    .output()
    .catch(() => fail(`could not run \`gh\`; the GitHub CLI must be installed for remote-release`));
  return {
    code: out.code,
    stdout: new TextDecoder().decode(out.stdout).trim(),
    stderr: new TextDecoder().decode(out.stderr).trim(),
  };
}

/** Resolve a remote branch head SHA via the refs API, or null when the branch
 *  does not exist (a 404). */
async function remoteSha(branch: string): Promise<string | null> {
  const out = await gh([
    "api",
    `repos/${repoPath()}/git/ref/heads/${branch}`,
    "--jq",
    ".object.sha",
  ]);
  if (out.code !== 0) return null;
  return out.stdout;
}

/** The remote branches a channel transfers between: latest <- main,
 *  stable <- latest. */
export function remoteTransfer(channel: ReleaseChannel): { source: string; target: string } {
  return channel === "latest"
    ? { source: "main", target: "latest" }
    : { source: "latest", target: "stable" };
}

/** Fast-forward the remote target branch onto the remote source branch,
 *  server-side. No-op when already equal. Refuses a non-fast-forward. Moving the
 *  branch is what triggers the CI release workflow. */
export async function fastForwardRemote(channel: ReleaseChannel): Promise<void> {
  const { source, target } = remoteTransfer(channel);
  step(`Remote fast-forward: ${target} <- ${source}`);

  const sourceSha = await remoteSha(source);
  if (!sourceSha) fail(`remote branch ${source} not found; cannot promote to ${target}.`);
  const targetSha = await remoteSha(target);

  if (targetSha === sourceSha) {
    ok(`remote ${target} already == ${source} (${sourceSha.slice(0, 12)}); nothing to do`);
    return;
  }

  if (targetSha) {
    // Refuse anything that isn't a clean fast-forward: source must be ahead of
    // (a descendant of) target.
    const cmp = await gh([
      "api",
      `repos/${repoPath()}/compare/${targetSha}...${sourceSha}`,
      "--jq",
      ".status",
    ]);
    if (cmp.code !== 0) fail(`could not compare ${target}...${source}: ${cmp.stderr}`);
    if (cmp.stdout !== "ahead" && cmp.stdout !== "identical") {
      fail(
        `remote ${source} is not a fast-forward of ${target} (status: ${cmp.stdout}); ` +
          `refusing to move ${target}.`,
      );
    }
    const patch = await gh([
      "api",
      "-X",
      "PATCH",
      `repos/${repoPath()}/git/refs/heads/${target}`,
      "-f",
      `sha=${sourceSha}`,
      "-F",
      "force=false",
    ]);
    if (patch.code !== 0) fail(`failed to move ${target} to ${source}: ${patch.stderr}`);
  } else {
    // Target branch does not exist yet: create it at the source commit.
    const create = await gh([
      "api",
      "-X",
      "POST",
      `repos/${repoPath()}/git/refs`,
      "-f",
      `ref=refs/heads/${target}`,
      "-f",
      `sha=${sourceSha}`,
    ]);
    if (create.code !== 0) fail(`failed to create ${target} at ${source}: ${create.stderr}`);
  }
  ok(
    `remote ${target} -> ${sourceSha.slice(0, 12)} ` +
      `(was ${targetSha ? targetSha.slice(0, 12) : "absent"})`,
  );
}
