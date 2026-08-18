// Module broker gating: grant states, undeclared policy, memories
// read/write coverage, and the db declaration gate. Op dispatch behavior is
// covered by the per-module suites; here a gated request that reaches
// dispatch (clean resolve, or its arg-validation / unknown-op error) is the
// "gate passed" signal.

import { assertEquals, assertRejects } from "@std/assert";
import type { PermissionDecl } from "@loca/shared";
import { permissionKey } from "@loca/shared";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { extensionsRegistry } from "../extensions/registry.ts";
import { AppError } from "@loca/core-engine";
import { memoriesStore } from "@loca/core-engine/services/memories-store";
import {
  deleteExtensionData,
  handleModuleRequest,
  type ModulePrompt,
  type ModuleRequest,
} from "./module-broker.ts";

const EXTENSION_ID = "example-extension";
const TOOL_NAME = "do_thing";

function seedExtension(
  opts: {
    id?: string;
    permissions?: PermissionDecl[];
    undeclaredPolicy?: "deny" | "ask";
    hasDatabase?: boolean;
  } = {},
): void {
  const id = opts.id ?? EXTENSION_ID;
  const r = extensionsRegistry();
  r.upsertExtension({
    id,
    source: "npm",
    displayName: "Example",
    version: "1.0.0",
    installedPath: "/tmp/example",
    manifestHash: "deadbeef",
    contentHash: "cafebabe",
    status: "installed",
    hasDatabase: opts.hasDatabase ?? false,
  });
  if (opts.undeclaredPolicy) r.setUndeclaredPolicy(id, opts.undeclaredPolicy);
  r.replaceTools(id, [
    {
      extensionId: id,
      name: TOOL_NAME,
      description: "does",
      parameters: { type: "object", properties: {} },
      triggers: [],
      fnExport: "doThing",
      alwaysAvailable: false,
      platforms: [],
      requiredPermissions: opts.permissions ?? [],
    },
  ]);
}

function setGrant(decl: PermissionDecl, state: "granted" | "ask" | "denied"): void {
  extensionsRegistry().setGrants(`${EXTENSION_ID}::${TOOL_NAME}`, [
    { key: permissionKey(decl), kind: decl.kind, state },
  ]);
}

// Runs a request with a scripted promptUser; returns the outcome plus the
// prompts the broker raised.
async function run(
  module: ModuleRequest["module"],
  op: string,
  allow: boolean,
): Promise<{ outcome: "dispatched" | "forbidden" | "other"; prompts: ModulePrompt[] }> {
  const prompts: ModulePrompt[] = [];
  try {
    await handleModuleRequest({
      extensionId: EXTENSION_ID,
      toolName: TOOL_NAME,
      callId: "call-1",
      module,
      op,
      args: {},
      promptUser: (p) => {
        prompts.push(p);
        return Promise.resolve(allow);
      },
    });
    return { outcome: "dispatched", prompts };
  } catch (err) {
    if (err instanceof AppError && err.code === "forbidden") {
      return { outcome: "forbidden", prompts };
    }
    if (
      err instanceof AppError &&
      (err.code === "validation_error" || err.code === "server_unavailable")
    ) {
      // Dispatch-level error (arg validation, unknown op, module turned
      // off, sidecar unreachable): the gate passed.
      return { outcome: "dispatched", prompts };
    }
    return { outcome: "other", prompts };
  }
}

const LLM_DECL: PermissionDecl = { kind: "llm", reason: "summarize things" };
const DOCS_READ: PermissionDecl = {
  kind: "memories",
  access: "read",
  reason: "read notes",
};
const DOCS_WRITE: PermissionDecl = {
  kind: "memories",
  access: "write",
  reason: "edit notes",
};

Deno.test("broker: granted state dispatches without prompting", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [LLM_DECL] });
    setGrant(LLM_DECL, "granted");
    const { outcome, prompts } = await run("llm", "complete", false);
    assertEquals(outcome, "dispatched");
    assertEquals(prompts.length, 0);
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: ask state prompts with the declared reason", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [LLM_DECL] });
    // No grant row at all behaves as ask.
    const accepted = await run("llm", "complete", true);
    assertEquals(accepted.outcome, "dispatched");
    assertEquals(accepted.prompts, [
      {
        permission: "llm",
        resource: "",
        declared: true,
        reason: "summarize things",
      },
    ]);
    const rejected = await run("llm", "complete", false);
    assertEquals(rejected.outcome, "forbidden");
    assertEquals(rejected.prompts.length, 1);
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: denied state rejects without prompting", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [LLM_DECL] });
    setGrant(LLM_DECL, "denied");
    const { outcome, prompts } = await run("llm", "complete", true);
    assertEquals(outcome, "forbidden");
    assertEquals(prompts.length, 0);
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: undeclared follows the extension policy", async () => {
  const env = await setupTestEnv();
  try {
    // Default policy is deny: reject without prompting.
    seedExtension();
    const denied = await run("tts", "speak", true);
    assertEquals(denied.outcome, "forbidden");
    assertEquals(denied.prompts.length, 0);
    // Policy ask: forward as undeclared.
    seedExtension({ undeclaredPolicy: "ask" });
    const asked = await run("tts", "speak", true);
    assertEquals(asked.outcome, "dispatched");
    assertEquals(asked.prompts, [
      {
        permission: "tts",
        resource: "",
        declared: false,
      },
    ]);
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: memories write grant covers read ops, not vice versa", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [DOCS_WRITE] });
    setGrant(DOCS_WRITE, "granted");
    const read = await run("memories", "get", false);
    assertEquals(read.outcome, "dispatched");
    assertEquals(read.prompts.length, 0);

    // A read-only declaration leaves write ops undeclared (policy deny).
    seedExtension({ permissions: [DOCS_READ] });
    setGrant(DOCS_READ, "granted");
    const write = await run("memories", "write", true);
    assertEquals(write.outcome, "forbidden");
    assertEquals(write.prompts.length, 0);
    // The read grant itself still works.
    const read2 = await run("memories", "list", false);
    assertEquals(read2.outcome, "dispatched");
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: a disabled memory is refused by write and edit, and leaks nothing", async () => {
  // Disabling is how a user takes a memory away from the model. `get`/`getFile`
  // already answered `not_found` for one, but `write` and `edit` both return the
  // prior content as `before` and neither checked the flag, so a tool could read
  // any disabled memory in full simply by writing to its title.
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [DOCS_WRITE] });
    setGrant(DOCS_WRITE, "granted");
    const call = (op: string, args: unknown) =>
      handleModuleRequest({
        extensionId: EXTENSION_ID,
        toolName: TOOL_NAME,
        callId: "call-1",
        module: "memories",
        op,
        args,
        promptUser: () => Promise.resolve(false),
      });

    await call("write", { title: "Secrets", content: "the private body" });
    const row = (await memoriesStore().list()).find((m) => m.title === "Secrets");
    memoriesStore().setEnabled(row!.id, false);

    // The content must not appear in any error or result. Asserted on the whole
    // thrown value rather than the code alone: a message that interpolated the
    // body would satisfy a code-only assertion.
    for (const [op, args] of [
      ["write", { title: "Secrets", content: "overwrite" }],
      ["edit", { title: "Secrets", find: "private", replace: "public" }],
    ] as const) {
      const err = await call(op, args).then(
        () => null,
        (e: unknown) => e as AppError,
      );
      assertEquals(err?.code, "forbidden", `${op} did not refuse a disabled memory`);
      assertEquals(
        JSON.stringify(err).includes("the private body"),
        false,
        `${op} leaked the disabled memory's content`,
      );
    }

    // Re-enabling restores normal behaviour, so the refusal is about the flag
    // and not about the title.
    memoriesStore().setEnabled(row!.id, true);
    assertEquals(await call("get", { title: "Secrets" }), {
      title: "Secrets",
      content: "the private body",
    });
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: memories ops round-trip through the store", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ permissions: [DOCS_WRITE] });
    setGrant(DOCS_WRITE, "granted");
    const call = (op: string, args: unknown) =>
      handleModuleRequest({
        extensionId: EXTENSION_ID,
        toolName: TOOL_NAME,
        callId: "call-1",
        module: "memories",
        op,
        args,
        promptUser: () => Promise.resolve(false),
      });

    const created = await call("write", { title: "Notes", content: "alpha" });
    assertEquals(created, {
      title: "Notes",
      before: "",
      after: "alpha",
      created: true,
    });
    const replaced = await call("write", { title: "Notes", content: "beta" });
    assertEquals(replaced, {
      title: "Notes",
      before: "alpha",
      after: "beta",
      created: false,
    });
    const edited = await call("edit", {
      title: "Notes",
      find: "beta",
      replace: "gamma",
    });
    assertEquals(edited, { title: "Notes", before: "beta", after: "gamma" });
    assertEquals(await call("get", { title: "Notes" }), {
      title: "Notes",
      content: "gamma",
    });
    const listing = (await call("list", {})) as Array<{ title: string; kind: string }>;
    assertEquals(
      listing.map((d) => [d.title, d.kind]),
      [["Notes", "knowledge"]],
    );

    // Missing memories and missing args surface as clear errors.
    await assertRejects(() => call("get", { title: "Nope" }), AppError, "not found");
    await assertRejects(() => call("write", { title: "X" }), AppError, "content");
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: db is gated by the extension database declaration", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ hasDatabase: false });
    const blocked = await run("db", "query", true);
    assertEquals(blocked.outcome, "forbidden");
    assertEquals(blocked.prompts.length, 0);

    seedExtension({ hasDatabase: true });
    const allowed = await run("db", "query", true);
    assertEquals(allowed.outcome, "dispatched");
    assertEquals(allowed.prompts.length, 0);
  } finally {
    await env.teardown();
  }
});

Deno.test("broker: db rejects ATTACH/DETACH and VACUUM INTO (sandbox escape)", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ hasDatabase: true });
    const call = (sql: string) =>
      handleModuleRequest({
        extensionId: EXTENSION_ID,
        toolName: TOOL_NAME,
        callId: "call-1",
        module: "db",
        op: "execute",
        args: { sql, params: [] },
        promptUser: () => Promise.resolve(false),
      });
    for (const sql of [
      "ATTACH DATABASE 'x.db' AS x",
      "attach database 'x.db' as x",
      "  DETACH DATABASE x",
      "VACUUM main INTO 'out.db'",
    ]) {
      await assertRejects(() => call(sql), AppError, "not allowed");
    }
    // A literal containing the word "attach" is fine: the screen strips
    // string contents before matching keywords.
    await call("CREATE TABLE notes (body TEXT)");
    const ok = await call("INSERT INTO notes (body) VALUES ('please attach the file')");
    assertEquals((ok as { changes: number }).changes, 1);
  } finally {
    deleteExtensionData(EXTENSION_ID);
    await env.teardown();
  }
});

Deno.test("broker: db returns BigInt-magnitude integers as JSON-safe values", async () => {
  const env = await setupTestEnv();
  try {
    seedExtension({ hasDatabase: true });
    const call = (op: string, sql: string) =>
      handleModuleRequest({
        extensionId: EXTENSION_ID,
        toolName: TOOL_NAME,
        callId: "call-1",
        module: "db",
        op,
        args: { sql, params: [] },
        promptUser: () => Promise.resolve(false),
      });
    await call("execute", "CREATE TABLE big (n INTEGER)");
    // 2^60 exceeds MAX_SAFE_INTEGER, so int64 returns it as a BigInt; the
    // broker must map it to a JSON-serializable form.
    await call("execute", "INSERT INTO big (n) VALUES (1152921504606846976)");
    const rows = (await call("query", "SELECT n FROM big")) as Array<{ n: unknown }>;
    const value = rows[0].n;
    assertEquals(typeof value === "number" || typeof value === "string", true);
    // Whatever the representation, it must survive JSON.stringify (the module
    // response is sent as JSON); a raw BigInt would throw here.
    JSON.stringify(rows);
  } finally {
    deleteExtensionData(EXTENSION_ID);
    await env.teardown();
  }
});

Deno.test("broker: db ops persist per extension and stay isolated", async () => {
  const env = await setupTestEnv();
  const A = "db-iso-a";
  const B = "db-iso-b";
  try {
    seedExtension({ id: A, hasDatabase: true });
    seedExtension({ id: B, hasDatabase: true });
    const call = (extensionId: string, op: string, args: unknown) =>
      handleModuleRequest({
        extensionId,
        toolName: TOOL_NAME,
        callId: "call-1",
        module: "db",
        op,
        args,
        promptUser: () => Promise.resolve(false),
      });

    await call(A, "execute", {
      sql: "CREATE TABLE notes (body TEXT)",
      params: [],
    });
    const inserted = await call(A, "execute", {
      sql: "INSERT INTO notes (body) VALUES (?)",
      params: ["hello"],
    });
    assertEquals((inserted as { changes: number }).changes, 1);
    assertEquals(await call(A, "query", { sql: "SELECT body FROM notes", params: [] }), [
      { body: "hello" },
    ]);

    // Extension B has its own database file: A's table does not exist there.
    await assertRejects(
      () => call(B, "query", { sql: "SELECT body FROM notes", params: [] }),
      Error,
      "no such table",
    );
  } finally {
    deleteExtensionData(A);
    deleteExtensionData(B);
    await env.teardown();
  }
});
