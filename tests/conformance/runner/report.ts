// Run output.
//
// Concise on success, specific on failure, and always explicit about what did
// NOT run. Two things a reader must never have to guess: which binary was
// exercised, and how much was skipped. With fifteen packages judged by this
// suite, a summary that reports only passes reads as full coverage, and
// "conformance is green" then means something it does not mean.

import type { Case, PackageId } from "./types.ts";
import type { CaseOutcome } from "./execute.ts";
import type { DiffOutcome } from "./diff.ts";
import type { ScopeRule } from "./scope.ts";

export interface SkipRecord {
  case: Case;
  reason: string;
}

export interface RunHeader {
  mode: string;
  target: string;
  binary: string;
  profile?: string;
  binaryMtime?: string;
  realHome?: string;
}

const useColor = !Deno.env.get("NO_COLOR") && Deno.stdout.isTerminal();
const color = (code: string, s: string) => (useColor ? `\x1b[${code}m${s}\x1b[0m` : s);
const green = (s: string) => color("32", s);
const red = (s: string) => color("31", s);
const yellow = (s: string) => color("33", s);
const dim = (s: string) => color("2", s);

export function printHeader(h: RunHeader): void {
  console.log(`conformance: mode=${h.mode} target=${h.target}`);
  // "Conformance passed" against a stale debug binary is a trap worth two lines
  // of output to close.
  const profile = h.profile ? ` (${h.profile})` : "";
  const stamp = h.binaryMtime ? dim(`  built ${h.binaryMtime}`) : "";
  console.log(`  binary: ${h.binary}${profile}${stamp}`);
  if (h.realHome) console.log(`  real home: ${h.realHome} ${dim("(copied, never mutated)")}`);
}

export function printCase(outcome: CaseOutcome, kind?: "cutover"): void {
  const ms = Math.round(outcome.durationMs);
  const tag = kind ? `${dim(`${kind} `)}` : "";
  // Notes print on pass as well as on failure. A broadcast that arrived just
  // inside its bound is a case that is quietly racing, and the only moment
  // anyone will act on that cheaply is while it is still green.
  const notes = outcome.steps.filter((s) => s.note);
  if (outcome.passed) {
    console.log(`${green("ok")}   ${tag}${outcome.case.id} ${dim(`(${ms}ms)`)}`);
    for (const s of notes) console.log(`     ${yellow("note")} step ${s.index + 1}: ${s.note}`);
    return;
  }
  console.log(`${red("FAIL")} ${tag}${outcome.case.id} ${dim(`(${ms}ms)`)}`);
  if (outcome.error) console.log(`       ${red(outcome.error)}`);
  for (const s of notes) console.log(`     ${yellow("note")} step ${s.index + 1}: ${s.note}`);
  for (const step of outcome.steps) {
    if (!step.mismatches.length) continue;
    console.log(`       step ${step.index + 1}: ${step.label}`);
    for (const m of step.mismatches.slice(0, 12)) {
      console.log(`         ${m.path}  ${m.detail}`);
    }
    if (step.mismatches.length > 12) {
      console.log(dim(`         ... and ${step.mismatches.length - 12} more`));
    }
  }
}

export function printDiffCase(d: DiffOutcome): void {
  const ms = Math.round(d.durationMs);
  switch (d.verdict) {
    case "agree":
      console.log(`${green("agree")}  ${d.case.id} ${dim(`(${ms}ms)`)}`);
      // Prose differences do not fail, but they are never hidden: a wording
      // divergence is still a divergence and somebody should see it.
      for (const m of d.prose) {
        console.log(`         ${dim("prose")} ${m.path}  ${m.detail}`);
      }
      return;
    case "differ": {
      // The interesting outcome: both cores satisfied the expectation and still
      // disagree, which means the expectation did not cover the difference.
      console.log(`${red("DIFFER")} ${d.case.id} ${dim(`(${ms}ms)`)}`);
      for (const m of d.mismatches.slice(0, 12)) {
        console.log(`         ${m.path}  ${m.detail}`);
      }
      if (d.mismatches.length > 12) {
        console.log(dim(`         ... and ${d.mismatches.length - 12} more`));
      }
      console.log(
        dim(
          "         if a field above is legitimately unstable, assert it with a " +
            "placeholder in the case rather than ignoring it here",
        ),
      );
      return;
    }
    case "one-failed": {
      const who = d.oracle.passed ? "rust" : "deno";
      console.log(`${red("FAIL")}   ${d.case.id} ${dim(`(${who} failed its expectation)`)}`);
      const failing = d.oracle.passed ? d.port : d.oracle;
      if (failing.error) console.log(`         ${red(failing.error)}`);
      for (const step of failing.steps) {
        for (const m of step.mismatches.slice(0, 8)) {
          console.log(`         ${step.label}  ${m.path}  ${m.detail}`);
        }
      }
      return;
    }
    case "both-failed":
      // Both failing usually means the CASE is wrong, not the port.
      console.log(
        `${yellow("BOTH")}   ${d.case.id} ${dim("(both cores failed; suspect the case)")}`,
      );
      for (const step of d.oracle.steps) {
        for (const m of step.mismatches.slice(0, 6)) {
          console.log(`         ${step.label}  ${m.path}  ${m.detail}`);
        }
      }
      if (d.oracle.error) console.log(`         ${red(d.oracle.error)}`);
      return;
  }
}

export interface CutoverContext {
  /** Cases that started on the Deno core and finished on the port target. */
  cases: CaseOutcome[];
  to: string;
}

interface SkipContext {
  skips: SkipRecord[];
  scopeReasons: ScopeRule[];
  strictScope: boolean;
  cutover: CutoverContext;
}

/** Cutover cases get their own block and their own count, deliberately.
 *
 *  They are run ONCE, on both binaries, whatever `--core` says, so they belong
 *  to neither of the two numbers everyone reads. Folded into "N passed" they
 *  would look like a case that passed on the core under test; folded into
 *  "N agree" they would look like a case that was replayed on both cores and
 *  matched, when nothing was compared at all. Either way the headline number
 *  would quietly come to mean less than it did. */
function printCutover(c: CutoverContext): boolean {
  if (!c.cases.length) return true;
  const failed = c.cases.filter((o) => !o.passed);
  const status = failed.length
    ? red(`${failed.length} failed`)
    : green(`all ${c.cases.length} passed`);
  console.log("");
  console.log(
    `  cutover  deno -> ${c.to}, run once, not compared: ${status}` +
      dim("  (does the install the oracle wrote still work on the port?)"),
  );
  for (const f of failed) console.log(red(`         ${f.case.id}`));
  return failed.length === 0;
}

/** What did NOT run, printed with the same prominence as what did. An unrun
 *  case is not a passing case, and the only way that stays true in people's
 *  heads is if the summary says so every time. */
function printSkips(ctx: SkipContext): boolean {
  if (!ctx.skips.length) return true;

  console.log("");
  console.log(yellow(`${ctx.skips.length} case(s) did NOT run:`));
  const byReason = new Map<string, Case[]>();
  for (const s of ctx.skips) {
    const list = byReason.get(s.reason) ?? [];
    list.push(s.case);
    byReason.set(s.reason, list);
  }
  for (const [reason, cases] of byReason) {
    console.log(`  ${yellow("skip")} ${reason}`);
    for (const c of cases) console.log(dim(`         ${c.id}`));
  }

  // A skip for a reason not in the scope table is a case quietly falling out of
  // coverage, which is the failure mode this whole section exists to prevent.
  const known = new Set(ctx.scopeReasons.map((r) => `${r.prefix}: ${r.reason}`));
  const unknown = [...byReason.keys()].filter((r) => !known.has(r));
  if (unknown.length) {
    console.log(red(`  ${unknown.length} skip reason(s) are NOT in the scope table:`));
    for (const r of unknown) console.log(red(`    ${r}`));
    return false;
  }
  if (ctx.strictScope) {
    console.log(red("  --strict-scope: any skip is a failure"));
    return false;
  }
  return true;
}

export interface Summary extends SkipContext {
  outcomes: CaseOutcome[];
}

/** Coverage is counted per ROUTE, not per case: that is the number the
 *  orchestrator reads at a package's diff review. */
export function printSummary(s: Summary): boolean {
  const passed = s.outcomes.filter((o) => o.passed);
  const failed = s.outcomes.filter((o) => !o.passed);

  const byPackage = new Map<PackageId, { pass: number; fail: number; routes: Set<string> }>();
  for (const o of s.outcomes) {
    const entry = byPackage.get(o.case.package) ?? { pass: 0, fail: 0, routes: new Set<string>() };
    if (o.passed) {
      entry.pass++;
      for (const r of o.case.routes) entry.routes.add(r);
    } else entry.fail++;
    byPackage.set(o.case.package, entry);
  }
  // A package with every case skipped has no row above; say so rather than
  // letting it vanish from the report entirely.
  const skippedOnly = new Set(s.skips.map((k) => k.case.package).filter((p) => !byPackage.has(p)));

  console.log("");
  for (const [pkg, entry] of [...byPackage].sort()) {
    const status = entry.fail ? red(`${entry.fail} failed`) : green("all passed");
    console.log(
      `  ${pkg.padEnd(4)} ${String(entry.pass).padStart(3)} passed  ${status}` +
        dim(`  ${entry.routes.size} route(s) covered`),
    );
  }
  for (const pkg of [...skippedOnly].sort()) {
    console.log(`  ${pkg.padEnd(4)} ${yellow("entirely skipped, 0 routes covered")}`);
  }

  const scopeOk = printSkips(s);
  const cutoverOk = printCutover(s.cutover);

  console.log("");
  const parts = [
    `${passed.length} passed`,
    failed.length ? red(`${failed.length} failed`) : "0 failed",
  ];
  if (s.skips.length) parts.push(yellow(`${s.skips.length} not run`));
  if (s.cutover.cases.length) parts.push(dim(`${s.cutover.cases.length} cutover`));
  console.log(parts.join(", "));

  if (failed.length) {
    console.log("");
    console.log("failed cases:");
    for (const f of failed) console.log(`  ${f.case.id}`);
  }
  return failed.length === 0 && scopeOk && cutoverOk;
}

export interface DiffSummary extends SkipContext {
  diffs: DiffOutcome[];
}

export function printDiffSummary(s: DiffSummary): boolean {
  const count = (v: DiffOutcome["verdict"]) => s.diffs.filter((d) => d.verdict === v).length;
  const agree = count("agree");
  const differ = count("differ");
  const oneFailed = count("one-failed");
  const bothFailed = count("both-failed");

  const byPackage = new Map<PackageId, { agree: number; bad: number; routes: Set<string> }>();
  for (const d of s.diffs) {
    const entry = byPackage.get(d.case.package) ?? { agree: 0, bad: 0, routes: new Set<string>() };
    if (d.verdict === "agree") {
      entry.agree++;
      for (const r of d.case.routes) entry.routes.add(r);
    } else entry.bad++;
    byPackage.set(d.case.package, entry);
  }

  console.log("");
  for (const [pkg, entry] of [...byPackage].sort()) {
    const status = entry.bad ? red(`${entry.bad} not agreeing`) : green("both cores agree");
    console.log(
      `  ${pkg.padEnd(4)} ${String(entry.agree).padStart(3)} agree  ${status}` +
        dim(`  ${entry.routes.size} route(s) covered`),
    );
  }

  const scopeOk = printSkips(s);
  const cutoverOk = printCutover(s.cutover);

  console.log("");
  const proseCount = s.diffs.reduce((n, d) => n + d.prose.length, 0);
  const parts = [green(`${agree} agree`)];
  if (proseCount) {
    parts.push(dim(`${proseCount} wording difference(s), not failing`));
  }
  if (differ) parts.push(red(`${differ} differ`));
  if (oneFailed) parts.push(red(`${oneFailed} one-sided failure`));
  if (bothFailed) parts.push(yellow(`${bothFailed} failed on both (suspect the case)`));
  if (s.skips.length) parts.push(yellow(`${s.skips.length} not run`));
  if (s.cutover.cases.length) parts.push(dim(`${s.cutover.cases.length} cutover`));
  console.log(parts.join(", "));

  const bad = s.diffs.filter((d) => d.verdict !== "agree");
  if (bad.length) {
    console.log("");
    console.log("not agreeing:");
    for (const d of bad) console.log(`  ${d.case.id} ${dim(`(${d.verdict})`)}`);
  }
  return bad.length === 0 && scopeOk && cutoverOk;
}
