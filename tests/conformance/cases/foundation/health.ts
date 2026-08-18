import type { Case } from "../../runner/types.ts";
import { UNSTABLE_CAPABILITIES } from "./capabilities.ts";

export const cases: Case[] = [
  {
    id: "f6.health.unauthenticated",
    package: "F6",
    routes: ["GET /api/v1/health"],
    auth: "none",
    steps: [
      {
        request: { method: "GET", path: "/api/v1/health" },
        expect: {
          status: 200,
          body: {
            status: "ok",
            version: "<version>",
            uptimeMs: "<duration>",
            core: { status: "<string>", subsystems: [] },
            adminPasswordSet: false,
            behindProxy: false,
            // Both targets under test are DESKTOP SERVICE cores, so the five
            // frontend-decided flags are asserted exactly: a core with a
            // listener can pair, schedule, spawn a stdio server, run tool
            // workers and replace its own binary. An embedded core is not
            // reachable from this harness (it has no listener to connect to by
            // definition), which is why the embedded answers are pinned by the
            // Rust unit tests in transport/health.rs instead.
            capabilities: {
              extensions: true,
              scheduledPrompts: true,
              stdioMcp: true,
              pairing: true,
              coreUpdate: true,
              // The two the cores legitimately disagree on; see the constant.
              ...UNSTABLE_CAPABILITIES,
            },
          },
        },
      },
    ],
  },
  {
    id: "f6.health.ignores-credentials",
    package: "F6",
    routes: ["GET /api/v1/health"],
    auth: "none",
    steps: [
      {
        // Health is the pre-pairing probe: the Client calls it to read
        // behindProxy BEFORE it has any credentials, so a garbage bearer must
        // not turn it into a 401.
        request: {
          method: "GET",
          path: "/api/v1/health",
          headers: { authorization: "Bearer not-a-real-token" },
        },
        // uptimeMs is named even though this case is about auth: an unmentioned
        // field is compared exactly in differential mode, so declaring it
        // unstable here is what stops two cores disagreeing about a clock.
        expect: {
          status: 200,
          bodyContains: {
            status: "ok",
            uptimeMs: "<duration>",
            capabilities: UNSTABLE_CAPABILITIES,
          },
        },
      },
    ],
  },
];
