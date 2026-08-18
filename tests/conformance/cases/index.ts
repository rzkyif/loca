// The case registry.
//
// Each package owns its own file (or files) under cases/, AND the two lines that
// register it here: the import and the spread. That carve-out is deliberate.
// Routing one mechanical line through the orchestrator would be fifteen
// round-trips to add fifteen imports, and a package whose cases are written but
// unregistered has shipped tests that never ran, which is worse than the
// ownership smudge. Everything else in this file is F8's.
//
// F8 owns cases/foundation/; every `d<N>-*.ts` belongs to the
// package that ported the slice, because the agent that just ported a route is
// the one who knows what its cases must assert, and a case written by anybody
// else tends to assert the implementation rather than the contract.

import { type Case, isCutoverCase } from "../runner/types.ts";
import { cases as health } from "./foundation/health.ts";
import { cases as admin } from "./foundation/admin.ts";
import { cases as pairing } from "./foundation/pairing.ts";
import { cases as ws } from "./foundation/ws.ts";
import { cases as parity } from "./foundation/parity.ts";
import { cases as cutover } from "./foundation/cutover.ts";
import { cases as d1Settings } from "./d1-settings.ts";
import { cases as d2Sessions } from "./d2-sessions.ts";
import { cases as d6Models } from "./d6-models.ts";
import { cases as d3Llm } from "./d3-llm.ts";
import { cases as d4Chat } from "./d4-chat.ts";
import { cases as d5Memories } from "./d5-memories.ts";
import { cases as d7Binaries } from "./d7-binaries.ts";
import { cases as d8Requirements } from "./d8-requirements.ts";
import { cases as d9Storage } from "./d9-storage.ts";
import { cases as d10Scheduled } from "./d10-scheduled-prompts.ts";
import { cases as d11Mcp } from "./d11-mcp.ts";
import { cases as d12Extensions } from "./d12-extensions.ts";
import { cases as d12Tools } from "./d12-tools.ts";
import { cases as d13Update } from "./d13-update.ts";
import { cases as d14Sidecars } from "./d14-sidecars.ts";
import { cases as d15Speech } from "./d15-speech.ts";

export const ALL_CASES: Case[] = [
  ...health,
  ...admin,
  ...pairing,
  ...ws,
  ...parity,
  ...cutover,
  ...d1Settings,
  ...d2Sessions,
  ...d6Models,
  ...d3Llm,
  ...d4Chat,
  ...d5Memories,
  ...d7Binaries,
  ...d8Requirements,
  ...d9Storage,
  ...d10Scheduled,
  ...d11Mcp,
  ...d12Extensions,
  ...d12Tools,
  ...d13Update,
  ...d14Sidecars,
  ...d15Speech,
];

/** Ids must be unique: they are the filter key and the report key, and a
 *  duplicate would silently shadow coverage. Checked at load. */
const seen = new Set<string>();
for (const c of ALL_CASES) {
  if (seen.has(c.id)) throw new Error(`duplicate conformance case id: ${c.id}`);
  seen.add(c.id);
  // A cutover case replaces the core underneath itself. Sharing that boot with
  // its neighbours would hand them a different core halfway through their own
  // steps, which is a failure nobody would attribute correctly.
  if (isCutoverCase(c) && !c.isolate) {
    throw new Error(`${c.id} uses handoff: "swap" and must set isolate: true`);
  }
}
