// Evaluate a math expression with expr-eval (a real parser, not JS eval).
// Covers arithmetic, comparisons, and the parser's built-in functions
// (sqrt, sin, log, min, max, ...).

import { Parser } from "expr-eval";
import type { ToolContext } from "./types.ts";

export function calculator(
  args: { expression?: string },
  _ctx: ToolContext,
): { expression: string; result: number | boolean } {
  const expression = typeof args?.expression === "string" ? args.expression.trim() : "";
  if (!expression) throw new Error("expression is required");

  let result: unknown;
  try {
    result = Parser.evaluate(expression);
  } catch (err) {
    throw new Error(
      `could not evaluate "${expression}": ${err instanceof Error ? err.message : String(err)}`,
    );
  }
  if (typeof result === "boolean") return { expression, result };
  if (typeof result !== "number" || !Number.isFinite(result)) {
    throw new Error("expression did not evaluate to a finite number");
  }
  return { expression, result };
}
