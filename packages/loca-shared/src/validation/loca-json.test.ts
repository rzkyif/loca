// loca.json is the manifest every extension MUST satisfy to load into loca.
// This is the test that locks down the validator against contract drift.

import { assertEquals } from "@std/assert";
import { extensionManifestSchema, parseExtensionManifest } from "./loca-json.ts";

const minimalTool = {
  name: "do-thing",
  description: "does a thing",
  parameters: { type: "object", properties: {} },
  function: "doThing",
};

const minimalManifest = {
  name: "@example/extension",
  displayName: "Example Extension",
  description: "example",
  tools: [minimalTool],
};

Deno.test("parseExtensionManifest: happy path returns ok with defaults applied", () => {
  const result = parseExtensionManifest(minimalManifest);
  assertEquals(result.ok, true);
  if (result.ok) {
    assertEquals(result.value.tools[0].triggers, []);
    assertEquals(result.value.tools[0].alwaysAvailable, false);
    assertEquals(result.value.tools[0].permissions.net, []);
    assertEquals(result.value.memories, []);
  }
});

Deno.test("parseExtensionManifest: requires a displayName", () => {
  const { displayName: _omit, ...noDisplay } = minimalManifest;
  assertEquals(parseExtensionManifest(noDisplay).ok, false);
});

Deno.test("parseExtensionManifest: an extension may ship only memories (no tools)", () => {
  const result = parseExtensionManifest({
    name: "mem-only",
    displayName: "Memory Pack",
    description: "memories only",
    tools: [],
    memories: [{ kind: "skill", path: "skills/file-bug" }],
  });
  assertEquals(result.ok, true);
});

Deno.test("parseExtensionManifest: rejects memory paths that escape the extension root", () => {
  for (const path of [
    "../secrets",
    "../../etc/passwd",
    "skills/../../escape",
    "/etc/passwd",
    "C:\\Windows\\system32",
    "skills\\..\\..\\escape",
  ]) {
    const result = parseExtensionManifest({
      name: "evil",
      displayName: "Evil",
      description: "tries to escape",
      memories: [{ kind: "knowledge", path }],
    });
    assertEquals(result.ok, false, `expected rejection for "${path}"`);
  }
});

Deno.test("parseExtensionManifest: accepts nested relative memory paths", () => {
  const result = parseExtensionManifest({
    name: "ok",
    displayName: "Ok",
    description: "nested but contained",
    memories: [{ kind: "skill", path: "skills/deep/file-bug" }],
  });
  assertEquals(result.ok, true);
});

Deno.test("parseExtensionManifest: rejects unknown top-level fields (strict mode)", () => {
  const result = parseExtensionManifest({ ...minimalManifest, extra: 1 });
  assertEquals(result.ok, false);
});

Deno.test("parseExtensionManifest: rejects duplicate tool names via superRefine", () => {
  const result = parseExtensionManifest({
    ...minimalManifest,
    tools: [minimalTool, { ...minimalTool }],
  });
  assertEquals(result.ok, false);
  if (!result.ok) {
    assertEquals(
      result.issues.some((i) => i.message.includes("duplicate tool name")),
      true,
    );
  }
});

Deno.test("parseExtensionManifest: tool name pattern is enforced", () => {
  for (const name of ["", "has spaces", "name-with-emoji-😀", "x".repeat(65)]) {
    const result = parseExtensionManifest({
      ...minimalManifest,
      tools: [{ ...minimalTool, name }],
    });
    assertEquals(result.ok, false, `should reject name: "${name}"`);
  }
});

Deno.test("parseExtensionManifest: env key must be SCREAMING_SNAKE_CASE", () => {
  const withEnv = (key: string) => ({
    ...minimalManifest,
    tools: [
      {
        ...minimalTool,
        permissions: {
          env: [{ key, reason: "needs it" }],
        },
      },
    ],
  });
  assertEquals(parseExtensionManifest(withEnv("OPENAI_API_KEY")).ok, true);
  assertEquals(parseExtensionManifest(withEnv("api_key")).ok, false);
  assertEquals(parseExtensionManifest(withEnv("kebab-case")).ok, false);
  // Leading digit is rejected by the regex.
  assertEquals(parseExtensionManifest(withEnv("1FOO")).ok, false);
});

Deno.test("parseExtensionManifest: net permission accepts numeric ports and '*'", () => {
  const result = parseExtensionManifest({
    ...minimalManifest,
    tools: [
      {
        ...minimalTool,
        permissions: {
          net: [{ host: "api.example.com", ports: [443, "*"], reason: "x" }],
        },
      },
    ],
  });
  assertEquals(result.ok, true);
});

Deno.test("parseExtensionManifest: net ports out of range are rejected", () => {
  const result = parseExtensionManifest({
    ...minimalManifest,
    tools: [
      {
        ...minimalTool,
        permissions: {
          net: [{ host: "x", ports: [70000], reason: "x" }],
        },
      },
    ],
  });
  assertEquals(result.ok, false);
});

Deno.test("extensionManifestSchema: defaults populate empty permission arrays", () => {
  const result = extensionManifestSchema.safeParse({
    name: "x",
    displayName: "X",
    description: "y",
    tools: [
      {
        name: "t",
        description: "d",
        parameters: {},
        function: "f",
      },
    ],
  });
  assertEquals(result.success, true);
  if (result.success) {
    assertEquals(result.data.tools[0].permissions.read, []);
    assertEquals(result.data.tools[0].permissions.run, []);
    assertEquals(result.data.tools[0].permissions.memories, []);
    assertEquals(result.data.tools[0].permissions.llm, []);
    assertEquals(result.data.database, false);
  }
});

Deno.test("parseExtensionManifest: module permissions and the database flag", () => {
  const result = parseExtensionManifest({
    ...minimalManifest,
    database: true,
    tools: [
      {
        ...minimalTool,
        permissions: {
          memories: [{ access: "write", reason: "save notes" }],
          llm: [{ reason: "summarize pages" }],
          tts: [{ reason: "speak results" }],
          stt: [{ reason: "transcribe clips" }],
        },
      },
    ],
  });
  assertEquals(result.ok, true);
  if (result.ok) {
    assertEquals(result.value.database, true);
    assertEquals(result.value.tools[0].permissions.memories, [
      { access: "write", reason: "save notes" },
    ]);
    assertEquals(result.value.tools[0].permissions.stt, [
      {
        reason: "transcribe clips",
      },
    ]);
  }
});

Deno.test("parseExtensionManifest: memories permission requires a valid access", () => {
  const result = parseExtensionManifest({
    ...minimalManifest,
    tools: [
      {
        ...minimalTool,
        permissions: { memories: [{ access: "admin", reason: "nope" }] },
      },
    ],
  });
  assertEquals(result.ok, false);
});
