// The quick model controls' level <-> setting translation. Focus on the
// context-relative thinking budgets and the snap-to-level selection. Pure
// functions, no I/O.

import { assertEquals } from "@std/assert";
import {
  creativitySelection,
  localThinkingBudget,
  thinkingLevelUpdates,
  thinkingSelection,
} from "./quick-controls.ts";

Deno.test("localThinkingBudget: scales with the context window (1/16, 1/8, 1/4)", () => {
  assertEquals(localThinkingBudget("low", 4096), 256);
  assertEquals(localThinkingBudget("medium", 4096), 512);
  assertEquals(localThinkingBudget("high", 4096), 1024);
  assertEquals(localThinkingBudget("high", 8192), 2048);
  assertEquals(localThinkingBudget("off", 4096), 0);
});

Deno.test("localThinkingBudget: falls back to the default context when unset", () => {
  assertEquals(localThinkingBudget("high", 0), localThinkingBudget("high", 4096));
});

Deno.test("thinkingLevelUpdates: local writes a context-scaled budget", () => {
  assertEquals(thinkingLevelUpdates("high", "local", 8192), {
    "llm.reasoning": "on",
    "llm.reasoningBudget": 2048,
  });
  assertEquals(thinkingLevelUpdates("off", "local", 8192), {
    "llm.reasoning": "off",
  });
});

Deno.test("thinkingLevelUpdates: external writes an effort level, no budget", () => {
  assertEquals(thinkingLevelUpdates("medium", "external", 8192), {
    "llm.reasoning": "on",
    "llm.reasoningEffort": "medium",
  });
});

Deno.test("thinkingSelection: snaps to the level matching the budget at the context size", () => {
  const base = { "llm.reasoning": "on", "llm.contextSize": 4096 };
  assertEquals(thinkingSelection({ ...base, "llm.reasoningBudget": 256 }, "local").value, "low");
  assertEquals(thinkingSelection({ ...base, "llm.reasoningBudget": 1024 }, "local").value, "high");
});

Deno.test("thinkingSelection: a non-matching budget is a custom 'N tokens' label", () => {
  const sel = thinkingSelection(
    {
      "llm.reasoning": "on",
      "llm.contextSize": 4096,
      "llm.reasoningBudget": 999,
    },
    "local",
  );
  assertEquals(sel.customLabel, "999 tokens");
});

Deno.test("thinkingSelection: an unset/zero budget reads as Unlimited", () => {
  const sel = thinkingSelection(
    {
      "llm.reasoning": "on",
      "llm.reasoningBudget": "",
    },
    "local",
  );
  assertEquals(sel.customLabel, "Unlimited");
});

Deno.test("thinkingSelection: reasoning off reads as off", () => {
  assertEquals(thinkingSelection({ "llm.reasoning": "off" }, "local").value, "off");
});

Deno.test("creativitySelection: a non-matching temperature is a custom 'X°' label", () => {
  assertEquals(creativitySelection({ "llm.temperature": 0.55 }).customLabel, "0.55°");
  assertEquals(creativitySelection({ "llm.temperature": 0.3 }).value, "precise");
});
