// Comparing a response to an expectation.
//
// The governing bias: anything not explicitly tolerated is compared EXACTLY,
// and an unexpected key is a failure. A port drops fields silently, and loose
// matching is how that ships. The tolerances are a closed set of placeholders
// plus two path-scoped escape hatches, so a case author cannot invent an ad-hoc
// regex that quietly matches everything.

export interface Mismatch {
  path: string;
  detail: string;
}

export interface NormaliseContext {
  /** Rewritten to "<port>" in the actual body, so a case can assert on a value
   *  that changes every run without giving up on the surrounding structure. */
  port: number;
  /** Rewritten to "<home>", so a case can write "<home>/sessions" and mean it. */
  home: string;
  /** The loopback mock's base URL, rewritten to "<upstream>". Each core under
   *  test gets its OWN upstream on its own port, so seeded settings that point
   *  at it differ between two cores for a reason that has nothing to do with
   *  either core. Folding it is what stops a harness artifact reading as a port
   *  divergence. */
  upstream: string;
  /** Lower bound for <ms>. The suite's start, minus an hour of slack. */
  startedAtMs: number;
}

const PLACEHOLDER = /^<([a-z0-9-]+)>$/;
const ESCAPED = /^<<(.+)>>$/;

type Check = (value: unknown, ctx: NormaliseContext) => string | null;

const ok = null;

function typeOf(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
}

function wrongType(want: string, v: unknown): string {
  return `expected ${want}, got ${typeOf(v)} ${JSON.stringify(v)}`;
}

// The closed matcher set. Adding one is a deliberate change to this file, not
// something a case can do inline.
const MATCHERS: Record<string, Check> = {
  id: (v) =>
    typeof v === "string" && /^[A-Za-z0-9_-]{8,}$/.test(v) ? ok : wrongType("an id string", v),
  uuid: (v) =>
    typeof v === "string" &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v)
      ? ok
      : wrongType("a uuid", v),
  // Bounded on purpose. An unbounded "any integer" passes on a core returning 0
  // or a seconds-based timestamp, which is the easy mistake to make in Rust
  // where SystemTime and chrono both make seconds the path of least resistance.
  ms: (v, ctx) => {
    if (typeof v !== "number" || !Number.isInteger(v)) return wrongType("integer epoch ms", v);
    if (v < ctx.startedAtMs) return `${v} is before the run started (seconds instead of ms?)`;
    if (v > Date.now() + 60_000) return `${v} is more than a minute in the future`;
    return ok;
  },
  // An expiry: epoch ms strictly in the future, and not absurdly far into it.
  // Same reasoning as <ms>, aimed the other way; a TTL that came back as
  // seconds, or as zero, is a real port bug and "any integer" would pass it.
  "ms-future": (v) => {
    if (typeof v !== "number" || !Number.isInteger(v)) return wrongType("integer epoch ms", v);
    if (v <= Date.now()) return `${v} is not in the future (seconds instead of ms?)`;
    if (v > Date.now() + 365 * 24 * 60 * 60 * 1000) return `${v} is more than a year out`;
    return ok;
  },
  duration: (v) =>
    typeof v === "number" && Number.isInteger(v) && v >= 0
      ? ok
      : wrongType("a non-negative integer duration", v),
  port: (v) =>
    (typeof v === "number" && v >= 1 && v <= 65535) || v === "<port>" ? ok : wrongType("a port", v),
  pin: (v) =>
    typeof v === "string" && /^[A-Za-z0-9+/]{43}=$/.test(v)
      ? ok
      : wrongType("base64 of 32 bytes", v),
  version: (v) =>
    typeof v === "string" && /^\d+\.\d+\.\d+/.test(v) ? ok : wrongType("a semver string", v),
  path: (v) =>
    typeof v === "string" &&
    (v.startsWith("/") || /^[A-Za-z]:[\\/]/.test(v) || v.includes("<home>"))
      ? ok
      : wrongType("an absolute path", v),
  sha256: (v) =>
    typeof v === "string" && /^[0-9a-f]{64}$/.test(v) ? ok : wrongType("a sha256", v),
  string: (v) => (typeof v === "string" ? ok : wrongType("a string", v)),
  number: (v) => (typeof v === "number" ? ok : wrongType("a number", v)),
  bool: (v) => (typeof v === "boolean" ? ok : wrongType("a boolean", v)),
  any: () => ok,
  // Handled by the caller, which is the only place that knows about presence.
  absent: () => "<absent> is only meaningful as an object value",
};

export function isPlaceholder(expected: unknown): string | null {
  if (typeof expected !== "string") return null;
  const m = PLACEHOLDER.exec(expected);
  if (!m) return null;
  return m[1] in MATCHERS ? m[1] : null;
}

/** Fold per-run values out of the actual body so the surrounding structure can
 *  still be asserted exactly. */
export function normalise(value: unknown, ctx: NormaliseContext): unknown {
  if (typeof value === "string") {
    let out = value;
    if (ctx.home) out = out.replaceAll(ctx.home, "<home>");
    if (ctx.upstream) out = out.replaceAll(ctx.upstream, "<upstream>");
    out = out.replaceAll(`:${ctx.port}`, ":<port>");
    return out;
  }
  if (Array.isArray(value)) return value.map((v) => normalise(v, ctx));
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) out[k] = normalise(v, ctx);
    return out;
  }
  return value;
}

function getPath(root: unknown, path: string): unknown {
  const parts = path
    .replace(/^\$\.?/, "")
    .split(".")
    .filter(Boolean);
  let cur = root;
  for (const p of parts) {
    if (cur === null || typeof cur !== "object") return undefined;
    cur = (cur as Record<string, unknown>)[p];
  }
  return cur;
}

function deletePath(root: unknown, path: string): void {
  const parts = path
    .replace(/^\$\.?/, "")
    .split(".")
    .filter(Boolean);
  const last = parts.pop();
  if (!last) return;
  const parent = parts.length ? getPath(root, parts.join(".")) : root;
  if (parent && typeof parent === "object") delete (parent as Record<string, unknown>)[last];
}

function sortBy(arr: unknown[], key: string): unknown[] {
  return [...arr].sort((a, b) => {
    const av = JSON.stringify((a as Record<string, unknown>)?.[key] ?? "");
    const bv = JSON.stringify((b as Record<string, unknown>)?.[key] ?? "");
    return av < bv ? -1 : av > bv ? 1 : 0;
  });
}

export interface CompareOptions {
  /** Subset comparison: extra keys in the actual object are allowed. */
  subset?: boolean;
  ignorePaths?: string[];
  unordered?: Record<string, string>;
}

/** Compare an actual value against an expectation. Returns every mismatch
 *  found rather than the first, because a port that shifted a shape usually
 *  produces several and fixing them one round trip at a time is miserable. */
export function compare(
  actual: unknown,
  expected: unknown,
  ctx: NormaliseContext,
  opts: CompareOptions = {},
): Mismatch[] {
  const a = structuredClone(normalise(actual, ctx));
  const e = structuredClone(expected);
  for (const p of opts.ignorePaths ?? []) {
    deletePath(a, p);
    deletePath(e, p);
  }
  for (const [p, key] of Object.entries(opts.unordered ?? {})) {
    const av = getPath(a, p);
    const ev = getPath(e, p);
    if (Array.isArray(av)) setPath(a, p, sortBy(av, key));
    if (Array.isArray(ev)) setPath(e, p, sortBy(ev, key));
  }
  const out: Mismatch[] = [];
  walk(a, e, "$", ctx, !!opts.subset, out);
  return out;
}

function setPath(root: unknown, path: string, value: unknown): void {
  const parts = path
    .replace(/^\$\.?/, "")
    .split(".")
    .filter(Boolean);
  const last = parts.pop();
  if (!last) return;
  const parent = parts.length ? getPath(root, parts.join(".")) : root;
  if (parent && typeof parent === "object") (parent as Record<string, unknown>)[last] = value;
}

function walk(
  actual: unknown,
  expected: unknown,
  path: string,
  ctx: NormaliseContext,
  subset: boolean,
  out: Mismatch[],
): void {
  const kind = isPlaceholder(expected);
  if (kind) {
    const err = MATCHERS[kind](actual, ctx);
    if (err) out.push({ path, detail: err });
    return;
  }
  if (typeof expected === "string") {
    const esc = ESCAPED.exec(expected);
    if (esc) expected = `<${esc[1]}>`;
  }

  if (expected === null || typeof expected !== "object") {
    if (actual !== expected) {
      out.push({
        path,
        detail: `expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
      });
    }
    return;
  }

  if (Array.isArray(expected)) {
    if (!Array.isArray(actual)) {
      out.push({ path, detail: wrongType("an array", actual) });
      return;
    }
    if (!subset && actual.length !== expected.length) {
      out.push({ path, detail: `expected ${expected.length} items, got ${actual.length}` });
    }
    for (let i = 0; i < expected.length; i++) {
      if (i >= actual.length) {
        out.push({ path: `${path}[${i}]`, detail: "missing" });
        continue;
      }
      walk(actual[i], expected[i], `${path}[${i}]`, ctx, subset, out);
    }
    return;
  }

  if (actual === null || typeof actual !== "object" || Array.isArray(actual)) {
    out.push({ path, detail: wrongType("an object", actual) });
    return;
  }

  const av = actual as Record<string, unknown>;
  const ev = expected as Record<string, unknown>;
  for (const [k, want] of Object.entries(ev)) {
    const child = `${path}.${k}`;
    if (want === "<absent>") {
      if (k in av)
        out.push({ path: child, detail: `expected absent, got ${JSON.stringify(av[k])}` });
      continue;
    }
    if (!(k in av)) {
      out.push({ path: child, detail: `missing (expected ${JSON.stringify(want)})` });
      continue;
    }
    walk(av[k], want, child, ctx, subset, out);
  }
  // The load-bearing half: an unexpected key is a failure unless the case opted
  // into a subset comparison.
  if (!subset) {
    for (const k of Object.keys(av)) {
      if (k in ev) continue;
      out.push({ path: `${path}.${k}`, detail: `unexpected key: ${JSON.stringify(av[k])}` });
    }
  }
}

// --- differential mode ------------------------------------------------------

/** Replace, in an OBSERVED body, every position where the expectation held a
 *  placeholder, with that placeholder's token.
 *
 *  This is what makes a two-actuals comparison possible. In diff mode neither
 *  side is an expectation, so there is nothing to interpret as a matcher, and a
 *  generated id or a timestamp differs between the cores for entirely legitimate
 *  reasons. The case has already declared which fields those are, by matching
 *  them with a placeholder; masking both sides at exactly those positions leaves
 *  everything the case did NOT declare unstable to be compared exactly.
 *
 *  Fields the case never mentioned are compared exactly on purpose. That is the
 *  point of diff mode: it catches what nobody thought to assert. When such a
 *  field is legitimately unstable the diff reports it, and the fix is to tighten
 *  the case with a placeholder, which makes the next run quieter AND the case
 *  stronger. */
export function maskByExpectation(actual: unknown, expected: unknown): unknown {
  if (isPlaceholder(expected)) return expected;
  // A `${captured}` reference is a value the case took FROM THIS CORE earlier in
  // the run, so cross-core it can never be equal and its inequality says nothing.
  // The assertion it carries is still made, within each core's own run: that is
  // where "the id survived the restart" is actually checked.
  if (typeof expected === "string" && /\$\{[A-Za-z0-9_]+\}/.test(expected)) return "<captured>";
  if (expected === null || typeof expected !== "object") return actual;

  if (Array.isArray(expected)) {
    if (!Array.isArray(actual)) return actual;
    return actual.map((v, i) => (i < expected.length ? maskByExpectation(v, expected[i]) : v));
  }
  if (actual === null || typeof actual !== "object" || Array.isArray(actual)) return actual;

  const av = actual as Record<string, unknown>;
  const ev = expected as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(av)) {
    out[k] = k in ev ? maskByExpectation(v, ev[k]) : v;
  }
  return out;
}

/** Plain structural equality between two observed bodies. No placeholder
 *  semantics: both sides are actuals, already masked and normalised. `aLabel`
 *  and `bLabel` name the two cores so a diff line reads without a legend. */
export function diffObserved(
  a: unknown,
  b: unknown,
  aLabel: string,
  bLabel: string,
  path = "$",
): Mismatch[] {
  if (a === null || typeof a !== "object" || b === null || typeof b !== "object") {
    if (a !== b) {
      return [
        {
          path,
          detail: `${aLabel} ${JSON.stringify(a)} vs ${bLabel} ${JSON.stringify(b)}`,
        },
      ];
    }
    return [];
  }
  if (Array.isArray(a) !== Array.isArray(b)) {
    return [{ path, detail: `${aLabel} ${typeOf(a)} vs ${bLabel} ${typeOf(b)}` }];
  }
  const out: Mismatch[] = [];
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) {
      out.push({
        path,
        detail: `${aLabel} has ${a.length} item(s), ${bLabel} has ${b.length}`,
      });
    }
    for (let i = 0; i < Math.min(a.length, b.length); i++) {
      out.push(...diffObserved(a[i], b[i], aLabel, bLabel, `${path}[${i}]`));
    }
    return out;
  }
  const av = a as Record<string, unknown>;
  const bv = b as Record<string, unknown>;
  for (const k of new Set([...Object.keys(av), ...Object.keys(bv)])) {
    const child = `${path}.${k}`;
    if (!(k in av)) {
      out.push({ path: child, detail: `only ${bLabel} has it: ${JSON.stringify(bv[k])}` });
      continue;
    }
    if (!(k in bv)) {
      out.push({ path: child, detail: `only ${aLabel} has it: ${JSON.stringify(av[k])}` });
      continue;
    }
    out.push(...diffObserved(av[k], bv[k], aLabel, bLabel, child));
  }
  return out;
}
