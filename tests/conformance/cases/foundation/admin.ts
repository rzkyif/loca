import type { Case } from "../../runner/types.ts";
import { UNSTABLE_CAPABILITIES } from "./capabilities.ts";

export const cases: Case[] = [
  {
    id: "f7.admin.password-needs-the-admin-token",
    package: "F7",
    routes: ["POST /api/v1/admin/password"],
    auth: "none",
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/admin/password",
          body: { password: "correct horse battery staple" },
        },
        expect: { status: 401, bodyContains: { error: { code: "admin_token_required" } } },
      },
    ],
  },
  {
    id: "f7.admin.password-rejects-a-short-one",
    package: "F7",
    routes: ["POST /api/v1/admin/password"],
    auth: "admin",
    steps: [
      {
        // The contract's minimum is 8 characters. On the Rust side this check
        // is not hand-written: it falls out of deserializing the generated
        // inbound type, which carries Zod's .min(8) (see D-003). A 200 here
        // would mean the constraint was lost in the bridge.
        request: { method: "POST", path: "/api/v1/admin/password", body: { password: "short" } },
        expect: { status: 400, bodyContains: { error: { code: "validation_error" } } },
      },
    ],
  },
  {
    id: "f7.admin.password-set-shows-in-health",
    package: "F7",
    routes: ["POST /api/v1/admin/password", "GET /api/v1/health"],
    auth: "admin",
    // Setting the password changes global state that other cases read.
    isolate: true,
    steps: [
      {
        request: {
          method: "POST",
          path: "/api/v1/admin/password",
          body: { password: "correct horse battery staple" },
        },
        expect: { status: 204 },
      },
      {
        request: { method: "GET", path: "/api/v1/health", auth: "none" },
        expect: {
          status: 200,
          bodyContains: {
            adminPasswordSet: true,
            uptimeMs: "<duration>",
            capabilities: UNSTABLE_CAPABILITIES,
          },
        },
      },
    ],
  },
];
