// Differential mode: the same case against both cores, compared to the
// expectation AND to each other.
//
// Every package from here is judged by "does the Rust do what the Deno does".
// The expectations answer that by proxy, and only for what somebody thought to
// write down. This answers it directly, including for fields nobody asserted,
// which is where the divergences that survive to cutover actually live.
//
// The comparison runs on MASKED observations (see maskByExpectation): wherever
// the case matched with a placeholder, both sides are replaced by the
// placeholder token, so a generated id or a timestamp is not reported as a
// disagreement. Everything else is compared exactly.

import type { CoreUnderTest } from "../harness/core.ts";
import { diffObserved, maskByExpectation, type Mismatch, normalise } from "./match.ts";
import type { NormaliseContext } from "./match.ts";
import type { CaseOutcome } from "./execute.ts";
import type { Case } from "./types.ts";

export type Verdict = "agree" | "differ" | "one-failed" | "both-failed";

export interface DiffOutcome {
  case: Case;
  verdict: Verdict;
  oracle: CaseOutcome;
  port: CaseOutcome;
  mismatches: Mismatch[];
  /** Differences that are reported but do not fail the run. See `isProse`. */
  prose: Mismatch[];
  durationMs: number;
}

/** An error body's `message` is prose, not contract.
 *
 *  D-016 settled that the `code` is the machine-readable contract and is
 *  asserted exactly. The `message` beside it has no machine consumer: the
 *  Client switches on status and code, never on message text. Requiring two
 *  independent implementations to emit byte-identical English would be
 *  busywork that buys nothing, and it would push every package toward copying
 *  strings instead of writing good ones (COPY.md governs those separately).
 *
 *  So a message-only difference is REPORTED but does not fail. Not ignored:
 *  silently dropping a whole class of difference is how a suite starts meaning
 *  less than people think. A case that genuinely needs the wording pinned
 *  writes it in the expectation, and then both cores must match it to pass at
 *  all, so this never gets in the way of a deliberate assertion. */
function isProse(path: string): boolean {
  return path.endsWith(".error.message") || path.endsWith(".message");
}

function ctxFor(core: CoreUnderTest, startedAtMs: number): NormaliseContext {
  return { port: core.port, home: core.home, upstream: core.upstream.baseUrl, startedAtMs };
}

/** Compare two completed runs of the same case. */
export function compareRuns(
  kase: Case,
  oracle: CaseOutcome,
  port: CaseOutcome,
  oracleCore: CoreUnderTest,
  portCore: CoreUnderTest,
  startedAtMs: number,
): DiffOutcome {
  const durationMs = oracle.durationMs + port.durationMs;

  if (!oracle.passed && !port.passed) {
    return {
      case: kase,
      verdict: "both-failed",
      oracle,
      port,
      mismatches: [],
      prose: [],
      durationMs,
    };
  }
  if (!oracle.passed || !port.passed) {
    return {
      case: kase,
      verdict: "one-failed",
      oracle,
      port,
      mismatches: [],
      prose: [],
      durationMs,
    };
  }

  const ctxA = ctxFor(oracleCore, startedAtMs);
  const ctxB = ctxFor(portCore, startedAtMs);
  const mismatches: Mismatch[] = [];

  // Step counts can differ only if one side short-circuited on a failure, which
  // the passed checks above already excluded. Guard anyway: a silent zip would
  // compare step 3 against step 4 and blame the wrong route.
  if (oracle.steps.length !== port.steps.length) {
    mismatches.push({
      path: "$.steps",
      detail: `deno ran ${oracle.steps.length} step(s), rust ran ${port.steps.length}`,
    });
  }

  const n = Math.min(oracle.steps.length, port.steps.length);
  for (let i = 0; i < n; i++) {
    const a = oracle.steps[i];
    const b = port.steps[i];
    const expected = kase.steps[i]?.expect;
    const wanted = expected?.body ?? expected?.bodyContains ?? expected?.frames ?? expected?.sse;

    if (a.status !== b.status) {
      mismatches.push({
        path: `$.steps[${i}].status`,
        detail: `${a.label}: deno ${a.status} vs rust ${b.status}`,
      });
    }

    const maskedA = maskByExpectation(normalise(a.observed, ctxA), wanted);
    const maskedB = maskByExpectation(normalise(b.observed, ctxB), wanted);
    for (const m of diffObserved(maskedA, maskedB, "deno", "rust")) {
      mismatches.push({
        path: `$.steps[${i}]${m.path.slice(1)}`,
        detail: `${a.label}: ${m.detail}`,
      });
    }
  }

  const hard = mismatches.filter((m) => !isProse(m.path));
  const prose = mismatches.filter((m) => isProse(m.path));

  return {
    case: kase,
    verdict: hard.length ? "differ" : "agree",
    oracle,
    port,
    mismatches: hard,
    prose,
    durationMs,
  };
}
