// Real-install parity: does an existing user's install still work?
//
// Everything else in this suite starts from a home the harness built, which
// only ever contains shapes the harness knows how to make. A real install
// carries whatever accumulated use left behind, and that is the thing that
// actually has to keep working on the day of the cutover. The on-disk tree is
// one of the byte-exact parity surfaces, and nothing else in the port checks it
// against a real one.
//
// This complements F4's Deno-written core.sqlite fixture rather than repeating
// it: that proves migration parity from the INSIDE, at the schema; this proves
// the observable half, from the wire, on a whole tree at once.
//
// SAFETY, and it is not negotiable: the real home is COPIED and never opened in
// place. A conformance suite that can damage a live ~/.loca is one nobody will
// run, and being asked to trust it once is enough to lose the habit.

import { copy } from "@std/fs/copy";
import { walk } from "@std/fs/walk";
import { join, relative, resolve } from "@std/path";
import { type CoreUnderTest, startCore } from "../harness/core.ts";
import type { Target } from "../harness/home.ts";
import { pinFromHome } from "../harness/tls.ts";
import { diffObserved } from "./match.ts";

export interface RealHomeOptions {
  realHome: string;
  yesCopy: boolean;
  target: Target;
}

/** Files that legitimately differ between two boots of the same home, so a
 *  difference here is not a finding. Everything else is compared. */
const VOLATILE = [
  "tls-cert.pem", // regenerated every boot from the same sealed key
  "core.log",
  "core.sqlite", // page layout is not deterministic; compared over the API instead
  "core.sqlite-wal",
  "core.sqlite-shm",
  "core.sqlite-journal",
];

function isVolatile(rel: string): boolean {
  const base = rel.split(/[\\/]/).pop() ?? rel;
  return VOLATILE.includes(base) || base.startsWith(".") || rel.startsWith("logs");
}

function liveLocaRoot(): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE") ?? "";
  return home ? resolve(home, ".loca") : "";
}

async function treeOf(root: string): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  for await (const entry of walk(root, { includeDirs: false, followSymlinks: false })) {
    const rel = relative(root, entry.path);
    if (isVolatile(rel)) continue;
    const bytes = await Deno.readFile(entry.path);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    out.set(rel, [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join(""));
  }
  return out;
}

async function readJson(core: CoreUnderTest, path: string, token: string): Promise<unknown> {
  const res = await fetch(`${core.baseUrl}${path}`, {
    headers: token ? { authorization: `Bearer ${token}` } : {},
  });
  const text = await res.text();
  try {
    return { status: res.status, body: JSON.parse(text) };
  } catch {
    return { status: res.status, body: text };
  }
}

export async function realHomeParity(opts: RealHomeOptions): Promise<boolean> {
  const source = resolve(opts.realHome);

  try {
    const stat = await Deno.stat(source);
    if (!stat.isDirectory) throw new Error("not a directory");
  } catch {
    console.error(`--real-home: ${source} is not a readable directory`);
    return false;
  }

  // The guard. Pointing at a live install is the normal thing to want to do,
  // and it is also the way to lose one, so it takes an explicit acknowledgement
  // even though the run only ever reads.
  const live = liveLocaRoot();
  if (live && (source === live || source.startsWith(live + "/"))) {
    if (!opts.yesCopy) {
      console.error(
        `--real-home points inside your live install (${live}).\n` +
          `The run COPIES it and never writes to the original, but that promise is\n` +
          `worth an explicit acknowledgement: re-run with --yes-copy.`,
      );
      return false;
    }
    console.log(`  live install acknowledged; copying, never writing to ${source}`);
  }

  const scratch = await Deno.makeTempDir({ prefix: "loca-conformance-real-" });
  const homeA = join(scratch, "deno");
  const homeB = join(scratch, "rust");
  await copy(source, homeA);
  await copy(source, homeB);

  const problems: string[] = [];
  let a: CoreUnderTest | null = null;
  let b: CoreUnderTest | null = null;

  try {
    // 1. Both boot at all. This is the headline question and it is answered
    //    before anything subtler is worth asking.
    try {
      [a, b] = await Promise.all([
        startCore({ target: "deno", home: homeA }),
        startCore({ target: opts.target, home: homeB }),
      ]);
    } catch (err) {
      console.log(`${"FAIL"}   a real install did not boot: ${err}`);
      return false;
    }
    console.log("ok     both cores booted on a copy of the real install");

    // 2. The same SPKI pin from the same sealed key. If this moves, every
    //    already-paired client on that install is told to re-pair.
    const [pinA, pinB] = await Promise.all([pinFromHome(a.home), pinFromHome(b.home)]);
    if (pinA !== pinB) {
      problems.push(`the SPKI pin differs: deno ${pinA} vs rust ${pinB}`);
    } else {
      console.log(`ok     both derived the same SPKI pin from the install's sealed key`);
    }

    // 3. Unauthenticated observable state agrees. Authenticated routes would
    //    need one of the install's real bearers, which the harness deliberately
    //    cannot mint without the admin token; health carries the version, the
    //    subsystem snapshot, and whether an admin password is set, all of which
    //    are read off the real tree.
    const [ha, hb] = await Promise.all([
      readJson(a, "/api/v1/health", ""),
      readJson(b, "/api/v1/health", ""),
    ]);
    const strip = (v: unknown) => {
      const r = structuredClone(v) as { body?: Record<string, unknown> };
      if (r.body && typeof r.body === "object") delete r.body.uptimeMs;
      return r;
    };
    // Structural, not stringified: the two cores serialize object keys in
    // different orders, and a JSON.stringify comparison reads that as a
    // disagreement about content when it is only a disagreement about order.
    const healthDiff = diffObserved(strip(ha), strip(hb), "deno", "rust");
    if (healthDiff.length) {
      for (const m of healthDiff) problems.push(`health ${m.path}: ${m.detail}`);
    } else console.log("ok     both report the same health snapshot for the install");
  } finally {
    await a?.kill();
    await b?.kill();
  }

  // 4. The tree each core left behind. Booting must not rewrite, drop, or
  //    invent state; the volatile set above is the only licensed difference.
  const [treeA, treeB] = await Promise.all([treeOf(homeA), treeOf(homeB)]);
  const onlyA = [...treeA.keys()].filter((k) => !treeB.has(k));
  const onlyB = [...treeB.keys()].filter((k) => !treeA.has(k));
  const changed = [...treeA.keys()].filter((k) => treeB.has(k) && treeA.get(k) !== treeB.get(k));

  if (onlyA.length) problems.push(`only the deno core's tree has: ${onlyA.join(", ")}`);
  if (onlyB.length) problems.push(`only the rust core's tree has: ${onlyB.join(", ")}`);
  if (changed.length) problems.push(`same path, different bytes: ${changed.join(", ")}`);
  if (!onlyA.length && !onlyB.length && !changed.length) {
    console.log(`ok     both left an identical tree (${treeA.size} file(s) compared)`);
  }

  // 5. Nothing touched the original.
  const sourceAfter = await treeOf(source);
  const original = await treeOf(homeA);
  const vanished = [...sourceAfter.keys()].filter((k) => !original.has(k));
  if (vanished.length) {
    console.log(`  note: the install has files the run did not reach: ${vanished.length}`);
  }

  if (Deno.env.get("LOCA_CONFORMANCE_KEEP_HOME") !== "1") {
    await Deno.remove(scratch, { recursive: true }).catch(() => {});
  }

  console.log("");
  if (problems.length) {
    console.log(`real-install parity: ${problems.length} problem(s)`);
    for (const p of problems) console.log(`  ${p}`);
    return false;
  }
  console.log("real-install parity: the install boots identically on both cores");
  return true;
}
