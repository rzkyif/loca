// Minimal semver comparison shared by the signed-manifest consumers
// (core.json self-update, binaries.json, extension.json) for downgrade refusal.

/** Compare two semver strings (major.minor.patch). Returns -1 if a < b,
 *  0 if equal, 1 if a > b. Pre-release tags and build metadata are
 *  ignored. That is sufficient for downgrade detection where the manifest
 *  publishes release versions only. */
export function compareSemver(a: string, b: string): number {
  const parse = (v: string): [number, number, number] => {
    const core = v.split(/[-+]/)[0]; // drop prerelease + build
    const parts = core.split(".").map((p) => parseInt(p, 10));
    return [
      Number.isFinite(parts[0]) ? parts[0] : 0,
      Number.isFinite(parts[1]) ? parts[1] : 0,
      Number.isFinite(parts[2]) ? parts[2] : 0,
    ];
  };
  const [aMaj, aMin, aPat] = parse(a);
  const [bMaj, bMin, bPat] = parse(b);
  if (aMaj !== bMaj) return aMaj < bMaj ? -1 : 1;
  if (aMin !== bMin) return aMin < bMin ? -1 : 1;
  if (aPat !== bPat) return aPat < bPat ? -1 : 1;
  return 0;
}
