// Barrel export for @loca/shared.
// API contract types + validation schemas consumed by both loca-core and loca-client.

export * from "./api/errors.ts";
export * from "./api/http.ts";
export * from "./api/ws.ts";

export * from "./domain/session.ts";
export * from "./domain/extension.ts";
export * from "./domain/model.ts";
export * from "./domain/variant.ts";
export * from "./domain/core-status.ts";
export * from "./domain/catalog.ts";
export * from "./domain/recommend.ts";
export * from "./domain/quick-controls.ts";
export * from "./domain/preset-detect.ts";
export * from "./domain/storage.ts";
export * from "./domain/settings/index.ts";
export * from "./domain/prompts.ts";
export * from "./domain/scheduled-prompt.ts";
export * from "./domain/memory.ts";
export * from "./domain/skill-frontmatter.ts";
export * from "./domain/mcp.ts";

export * from "./validation/loca-json.ts";
export * from "./validation/pairing.ts";
export * from "./validation/chat.ts";
export * from "./validation/session.ts";
export * from "./validation/ws.ts";
export * from "./validation/scheduled-prompt.ts";

export * from "./crypto/pake.ts";
export * from "./crypto/canonical.ts";
export * from "./crypto/signature.ts";
