// The conformance runner.
//
//   deno task conformance                       every case, against the Deno core
//   deno task conformance --core=rust           against the Rust core
//   deno task conformance --core=diff           BOTH, compared to each other
//   deno task conformance --filter=D2           one package's slice
//   deno task conformance --list                enumerate without running
//   deno task conformance --core=diff --real-home=<path> --yes-copy
//                                               boot both on a COPY of a real install
//
// Run from the repo root via `deno task conformance`, which supplies
// --unsafely-ignore-certificate-errors=127.0.0.1. That flag is a decision this
// process makes about which certificates IT will accept, scoped to loopback.
// The core still serves its real self-signed cert with its real key, and the
// pin is still derived and folded for real (harness/tls.ts, harness/pair.ts),
// so certificate identity is checked; only chain validation is relaxed.

import { parseArgs } from "@std/cli/parse-args";
import { type CoreUnderTest, startCore } from "./harness/core.ts";
import type { Target } from "./harness/home.ts";
import { findRustCore, rustCoreStaleness } from "./harness/repo.ts";
import { ALL_CASES } from "./cases/index.ts";
import {
  type CaseOutcome,
  establishIdentities,
  type Identities,
  runCase,
} from "./runner/execute.ts";
import { compareRuns, type DiffOutcome } from "./runner/diff.ts";
import { outOfScope, scopeRules } from "./runner/scope.ts";
import {
  printCase,
  printDiffCase,
  printDiffSummary,
  printHeader,
  printSummary,
  type SkipRecord,
} from "./runner/report.ts";
import { realHomeParity } from "./runner/real-home.ts";
import { type Case, isCutoverCase } from "./runner/types.ts";

const args = parseArgs(Deno.args, {
  string: ["core", "filter", "json", "target", "real-home"],
  boolean: ["list", "strict-scope", "keep-home", "yes-copy"],
  default: { core: "deno" },
});

const CORE = String(args.core);
if (!["deno", "rust", "diff"].includes(CORE)) {
  console.error(`unknown --core=${CORE} (expected deno, rust, or diff)`);
  Deno.exit(2);
}

// The port target is a separate axis from which cores run. `rust-mobile` is the
// same binary with capabilities gated off, so it is chosen here, not inferred.
const portTarget: Target = args.target === "rust-mobile" ? "rust-mobile" : "rust-desktop";
if (args.target && !["rust-desktop", "rust-mobile"].includes(String(args.target))) {
  console.error(`unknown --target=${args.target} (expected rust-desktop or rust-mobile)`);
  Deno.exit(2);
}

if (args["keep-home"]) Deno.env.set("LOCA_CONFORMANCE_KEEP_HOME", "1");

function matchesFilter(kase: Case, filter: string | undefined): boolean {
  if (!filter) return true;
  const needle = filter.toLowerCase();
  return kase.id.toLowerCase().includes(needle) || kase.package.toLowerCase() === needle;
}

const selected = ALL_CASES.filter((c) => matchesFilter(c, args.filter));

if (args.list) {
  for (const c of selected) {
    console.log(`${c.package.padEnd(4)} ${c.id}${isCutoverCase(c) ? "  [cutover]" : ""}`);
    for (const r of c.routes) console.log(`       ${r}`);
  }
  console.log(`\n${selected.length} case(s)`);
  Deno.exit(0);
}

if (!selected.length) {
  console.error(`no cases matched --filter=${args.filter}`);
  Deno.exit(2);
}

// Fail loudly rather than skipping. A run that passes because nothing ran is
// exactly the failure this suite exists to prevent.
//
// A cutover case needs BOTH binaries in every mode, `--core=deno` included: it
// starts on the Deno core and hands the home to the Rust one, so it is not
// parameterised by which core the rest of the run is pointed at.
const cutoverSelected = selected.filter(isCutoverCase);
const needsRust = CORE === "rust" || CORE === "diff" || cutoverSelected.length > 0;
const rustBin = needsRust ? findRustCore() : null;
if (needsRust && !rustBin) {
  const why =
    CORE === "deno"
      ? `${cutoverSelected.length} cutover case(s) hand their home to the Rust core, so it is needed even with --core=deno`
      : `--core=${CORE} runs against it`;
  console.error(
    "the Rust core binary was not found at target/release/loca-core or " +
      `target/debug/loca-core.\n${why}.\n` +
      "Build it with `deno task build:core-rs` and retry. Refusing to skip: a " +
      "run that passes because nothing ran is the failure this suite prevents.",
  );
  Deno.exit(2);
}

// A stale binary is worse than a missing one: the run completes and reports
// numbers that describe the PREVIOUS build, so a change reads as verified when
// nothing exercised it. Refused rather than warned, for the same reason the
// missing-binary case above is refused.
const stale = rustBin ? rustCoreStaleness(rustBin) : null;
if (stale) {
  console.error(
    `the Rust core binary is STALE: ${stale.file} was modified at ` +
      `${stale.at.toISOString()}, after ${rustBin?.path} was built at ` +
      `${rustBin?.mtime?.toISOString()}.\n` +
      "Run `deno task build:core-rs` and retry. Refusing to run: this suite " +
      "does not build, so the result would describe the previous binary.",
  );
  Deno.exit(2);
}

printHeader({
  mode: CORE,
  target: CORE === "deno" ? "deno" : portTarget,
  binary: rustBin?.path ?? "packages/loca-core/src/main.ts (deno, from source)",
  profile: rustBin?.profile,
  binaryMtime: rustBin?.mtime?.toISOString(),
  realHome: args["real-home"] ? String(args["real-home"]) : undefined,
});
console.log("");

// --- real-install parity ----------------------------------------------------

if (args["real-home"]) {
  if (CORE !== "diff") {
    console.error("--real-home needs --core=diff: it compares the two cores on the same install");
    Deno.exit(2);
  }
  const ok = await realHomeParity({
    realHome: String(args["real-home"]),
    yesCopy: Boolean(args["yes-copy"]),
    target: portTarget,
  });
  Deno.exit(ok ? 0 : 1);
}

// --- scenario grouping ------------------------------------------------------

interface Group {
  key: string;
  cases: Case[];
  /** Boots the Deno core with a swap target and runs ONCE, whatever `--core`
   *  says. See `isCutoverCase`. */
  cutover?: boolean;
  /** `LOCA_CHANNEL` for this group's boot. Only an isolated case can set it,
   *  because a scenario shares one core and the channel is a property of the
   *  boot rather than of a request. */
  channel?: string;
  /** Files written into this group's home before its first boot. Only an
   *  isolated or cutover case sets it, for the same reason as `channel`. */
  plant?: Case["plant"];
}
const groups: Group[] = [];
const byScenario = new Map<string, Case[]>();
const skips: SkipRecord[] = [];

// Scope only applies to what the PORT serves; the Deno oracle serves everything.
const scopeTarget: Target | null = CORE === "deno" ? null : portTarget;

for (const kase of selected) {
  // A cutover case always ends up on the port target, so its scope is the port
  // target's even when the rest of the run is pointed at the oracle.
  const against = isCutoverCase(kase) ? portTarget : scopeTarget;
  const reason = against ? outOfScope(kase.routes, against) : null;
  if (reason) {
    skips.push({ case: kase, reason });
    continue;
  }
  if (isCutoverCase(kase)) {
    groups.push({
      key: `${kase.id} (cutover)`,
      cases: [kase],
      cutover: true,
      plant: kase.plant,
    });
    continue;
  }
  if (kase.isolate) {
    groups.push({
      key: `${kase.id} (isolated)`,
      cases: [kase],
      channel: kase.channel,
      plant: kase.plant,
    });
    continue;
  }
  if (kase.channel) {
    // Refused rather than ignored. A channel silently dropped because the case
    // shares a boot is the worst outcome available: the case runs on `dev`,
    // asserts a `stable` behaviour, and reports whatever the dev branch does.
    throw new Error(
      `${kase.id} sets channel: "${kase.channel}" without isolate: true. The channel is a ` +
        `property of the core boot, and a scenario shares one.`,
    );
  }
  if (kase.plant) {
    // Same reasoning as `channel`, and the failure is quieter. A planted file
    // silently dropped because the case shares a boot leaves the case asserting
    // against a home that never received its fixture, and "the model is not
    // listed" is a plausible-looking result rather than an obvious error.
    throw new Error(
      `${kase.id} sets plant without isolate: true. Planted files are a property of the ` +
        `core boot, and a scenario shares one.`,
    );
  }
  const key = kase.scenario ?? "default";
  const list = byScenario.get(key) ?? [];
  list.push(kase);
  byScenario.set(key, list);
}
for (const [key, cases] of byScenario) groups.push({ key, cases });

const startedAtMs = Date.now() - 60 * 60 * 1000;

function identityNeeds(cases: Case[]): { paired: boolean; other: boolean } {
  const other = cases.some(
    (c) => c.auth === "paired:other" || c.steps.some((s) => s.request?.auth === "paired:other"),
  );
  const paired =
    other ||
    cases.some(
      (c) =>
        c.auth.startsWith("paired") ||
        (c.watch?.length !== undefined && c.watch.length > 0) ||
        c.steps.some((s) => s.request?.auth?.startsWith("paired")) ||
        c.steps.some((s) => s.expect.ws),
    );
  return { paired, other };
}

async function identitiesFor(
  core: CoreUnderTest,
  needs: { paired: boolean; other: boolean },
): Promise<Identities> {
  if (!needs.paired) return { primary: { clientId: "", token: "", coreVersion: "" } };
  return await establishIdentities(core, needs.other);
}

// --- run --------------------------------------------------------------------

const outcomes: CaseOutcome[] = [];
const diffs: DiffOutcome[] = [];
/** Cutover cases, kept apart from both. They are run once in every mode, so
 *  folding them into `outcomes` would count a two-core case as a one-core pass,
 *  and folding them into `diffs` would count a case that was never compared as
 *  an "agree". Reported in their own block instead. */
const cutovers: CaseOutcome[] = [];

for (const group of groups) {
  const needs = identityNeeds(group.cases);
  let oracle: CoreUnderTest | null = null;
  let port: CoreUnderTest | null = null;

  try {
    if (group.cutover) {
      // Always Deno first, in every mode. The direction is not symmetric: a
      // home seeded for the Rust core carries one helper binary, and the Deno
      // core refuses to boot without four, so only deno -> rust is bootable at
      // all. It is also the only direction the cutover claim is about.
      oracle = await startCore({ target: "deno", swapTo: portTarget, plant: group.plant });
      const ids = await identitiesFor(oracle, needs);
      for (const kase of group.cases) {
        const outcome = await runCase(oracle, ids, kase, startedAtMs);
        cutovers.push(outcome);
        printCase(outcome, "cutover");
        if (!outcome.passed) {
          for (const line of oracle.recentLogs().slice(-6)) {
            if (line.trim()) console.log(`       core| ${line}`);
          }
        }
      }
    } else if (CORE === "diff") {
      // Boot both in parallel: two cold starts per scenario is the dominant
      // cost of diff mode, and they are independent.
      [oracle, port] = await Promise.all([
        startCore({ target: "deno", channel: group.channel }),
        startCore({ target: portTarget, channel: group.channel }),
      ]);
      const [oracleIds, portIds] = await Promise.all([
        identitiesFor(oracle, needs),
        identitiesFor(port, needs),
      ]);
      for (const kase of group.cases) {
        const a = await runCase(oracle, oracleIds, kase, startedAtMs);
        const b = await runCase(port, portIds, kase, startedAtMs);
        const d = compareRuns(kase, a, b, oracle, port, startedAtMs);
        diffs.push(d);
        printDiffCase(d);
      }
    } else {
      const target: Target = CORE === "deno" ? "deno" : portTarget;
      oracle = await startCore({ target, channel: group.channel, plant: group.plant });
      const ids = await identitiesFor(oracle, needs);
      for (const kase of group.cases) {
        const outcome = await runCase(oracle, ids, kase, startedAtMs);
        outcomes.push(outcome);
        printCase(outcome);
        if (!outcome.passed) {
          for (const line of oracle.recentLogs().slice(-6)) {
            if (line.trim()) console.log(`       core| ${line}`);
          }
        }
      }
    }
  } catch (err) {
    console.error(`could not run scenario "${group.key}": ${err}`);
    for (const kase of group.cases) {
      const failed: CaseOutcome = {
        case: kase,
        passed: false,
        steps: [],
        error: `scenario "${group.key}" failed to start`,
        durationMs: 0,
      };
      if (group.cutover) cutovers.push(failed);
      else if (CORE === "diff") {
        diffs.push({
          case: kase,
          verdict: "both-failed",
          oracle: failed,
          port: failed,
          mismatches: [],
          prose: [],
          durationMs: 0,
        });
      } else outcomes.push(failed);
    }
  } finally {
    await oracle?.stop();
    await port?.stop();
  }
}

// --- report -----------------------------------------------------------------

const scopeReasons = scopeTarget ? scopeRules(scopeTarget) : [];
const cutover = { cases: cutovers, to: portTarget };
const ok =
  CORE === "diff"
    ? printDiffSummary({
        diffs,
        cutover,
        skips,
        scopeReasons,
        strictScope: Boolean(args["strict-scope"]),
      })
    : printSummary({
        outcomes,
        cutover,
        skips,
        scopeReasons,
        strictScope: Boolean(args["strict-scope"]),
      });

if (args.json) {
  // Run once, on both binaries, in every mode: reported on its own axis so a
  // reader cannot mistake a cutover pass for a per-core pass or an agreement.
  const cutoverJson = cutovers.map((o) => ({
    id: o.case.id,
    package: o.case.package,
    routes: o.case.routes,
    from: "deno",
    to: portTarget,
    passed: o.passed,
    error: o.error,
  }));
  const payload =
    CORE === "diff"
      ? {
          mode: CORE,
          target: portTarget,
          binary: rustBin?.path,
          cutover: cutoverJson,
          cases: diffs.map((d) => ({
            id: d.case.id,
            package: d.case.package,
            routes: d.case.routes,
            verdict: d.verdict,
            oraclePassed: d.oracle.passed,
            portPassed: d.port.passed,
            mismatches: d.mismatches,
          })),
          skipped: skips.map((s) => ({ id: s.case.id, reason: s.reason })),
        }
      : {
          mode: CORE,
          target: CORE === "deno" ? "deno" : portTarget,
          binary: rustBin?.path ?? "deno-from-source",
          cutover: cutoverJson,
          cases: outcomes.map((o) => ({
            id: o.case.id,
            package: o.case.package,
            routes: o.case.routes,
            passed: o.passed,
            durationMs: Math.round(o.durationMs),
            error: o.error,
            mismatches: o.steps.flatMap((s) =>
              s.mismatches.map((m) => ({ step: s.index, label: s.label, ...m })),
            ),
          })),
          skipped: skips.map((s) => ({ id: s.case.id, reason: s.reason })),
        };
  await Deno.writeTextFile(args.json, JSON.stringify(payload, null, 2));
}

Deno.exit(ok ? 0 : 1);
