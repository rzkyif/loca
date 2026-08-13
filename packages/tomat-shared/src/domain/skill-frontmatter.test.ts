import { assertEquals } from "@std/assert";
import { parseSkill, serializeSkill } from "./skill-frontmatter.ts";

Deno.test("parseSkill: no frontmatter returns whole text as body", () => {
  const content = "# Title\n\nJust a body.";
  assertEquals(parseSkill(content), {
    description: "",
    suggestedTools: [],
    body: content,
  });
});

Deno.test("parseSkill: description + block-list tools, body without the blank line", () => {
  const content =
    '---\ndescription: "Summarize a thread"\nsuggested-tools:\n  - read_session\n  - search_messages\n---\n\n# Body\n\nDo the thing.';
  assertEquals(parseSkill(content), {
    description: "Summarize a thread",
    suggestedTools: ["read_session", "search_messages"],
    body: "# Body\n\nDo the thing.",
  });
});

Deno.test("parseSkill: inline-array tools and an ignored extra key", () => {
  const content = '---\nname: my-skill\nsuggested-tools: [a, "b", c]\n---\n\nBody.';
  assertEquals(parseSkill(content), {
    description: "",
    suggestedTools: ["a", "b", "c"],
    body: "Body.",
  });
});

Deno.test("serializeSkill: body-only when no description or tools", () => {
  assertEquals(serializeSkill({ description: "", suggestedTools: [], body: "# Body" }), "# Body");
});

Deno.test("serializeSkill: emits quoted description + block list", () => {
  assertEquals(
    serializeSkill({
      description: "Do X",
      suggestedTools: ["a", "b"],
      body: "Body.",
    }),
    '---\ndescription: "Do X"\nsuggested-tools:\n  - a\n  - b\n---\n\nBody.',
  );
});

Deno.test("round-trip: description with a colon and inner quotes survives", () => {
  const parts = {
    description: 'Note: keep "Tomat" capitalized',
    suggestedTools: ["read_session"],
    body: "# Body\n\ntext",
  };
  assertEquals(parseSkill(serializeSkill(parts)), parts);
});

Deno.test("round-trip: body-only skill survives", () => {
  const parts = { description: "", suggestedTools: [], body: "# Just a body\n" };
  assertEquals(parseSkill(serializeSkill(parts)), parts);
});
