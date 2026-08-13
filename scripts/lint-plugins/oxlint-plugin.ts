// Local oxlint plugin housing Loca's custom lint rules. Registered via
// `jsPlugins` in the root `.oxlintrc.json` under the `loca/` namespace.
//
// Rules:
//   - no-tauri-import: forbid direct `@tauri-apps/*` imports. The Svelte client
//     must reach Tauri capabilities through `$lib/platform/` (a Tauri impl plus
//     a browser stub), so the same component tree can run under a future
//     web/mobile build. Scoped to the client UI tree, with the one allowed file
//     (`platform/tauri.ts`) exempted, via `overrides` in `.oxlintrc.json`. oxlint
//     can't parse `.svelte`, so `check-tauri-imports-svelte.ts` covers the
//     `<script>`-block gap.
//   - no-em-dash: forbid the em dash (U+2014) anywhere: code, comments, or
//     string/template text. Scans the raw source so every occurrence is caught,
//     not just AST string nodes.
//   - no-builtin-palette-color: forbid the built-in UnoCSS/Tailwind palette
//     color utilities (a property prefix + a palette hue + a numeric shade, e.g.
//     `text-<hue>-<shade>`, `bg-<hue>-<shade>`, `focus:ring-<hue>-<shade>`).
//     Those paint a fixed sRGB color that ignores the user's appearance settings
//     and does NOT theme-invert in dark mode, so a color that reads in light
//     mode gets neutralized in dark. The themable tokens
//     (`*-accent-{blue|purple|red|green|yellow}-N`, `*-default-N`) resolve to
//     the per-scope CSS variables and carry dark-mode inversion for free. Like
//     no-em-dash, scans raw source so it catches class strings in `.ts` and
//     `.svelte`; check-builtin-palette-color.ts covers the rest. The
//     `*-accent-*` / `*-default-*` tokens are unaffected (their hue segment is
//     literally `accent`/`default`, never a palette name). This source keeps no
//     literal palette-color token, or the rule would flag its own examples.
//
// Keep this file em-dash-free, or the rule will flag its own source.

interface ImportDeclarationNode {
  source: { value: unknown };
}

// Loose AST-node shape covering the fields the rules read (name for Identifier,
// value for Literal, object/property/computed for MemberExpression, init for a
// VariableDeclarator). Everything is optional so an unexpected node just no-ops.
interface AstNode {
  type?: string;
  name?: string;
  value?: unknown;
  object?: AstNode;
  property?: AstNode;
  computed?: boolean;
  init?: AstNode | null;
}

interface MemberExpressionNode {
  object?: AstNode;
  property?: AstNode;
  computed?: boolean;
}

interface VariableDeclaratorNode {
  init?: AstNode | null;
}

interface ReportDescriptor {
  node?: unknown;
  loc?: { line: number; column: number };
  message: string;
}

interface RuleContext {
  sourceCode: { text: string };
  report(descriptor: ReportDescriptor): void;
}

const TAURI_MESSAGE =
  "Direct @tauri-apps import is forbidden here. Route this call through " +
  "$lib/platform/ so the web/mobile build stub can intercept it. Add a method " +
  "to the Platform interface if one doesn't exist yet.";

const EM_DASH = String.fromCharCode(0x2014);
const EM_DASH_MESSAGE =
  "Em dash (U+2014) is not allowed. Reword the surrounding text so the " +
  "sentence reads naturally without an em dash.";

// A built-in UnoCSS/Tailwind color utility: a color property prefix, a palette
// hue, and a numeric shade (e.g. `text-<hue>-<shade>`). The `*-accent-*` /
// `*-default-*` tokens never match: their hue segment is literally
// `accent`/`default`, not a palette name. KEEP IN SYNC with the copy in
// check-builtin-palette-color.ts (this file is .ts, lint-covered by oxlint; that
// one walks the file types oxlint can't parse).
const PALETTE_COLOR_RE =
  /\b(?:text|bg|border|ring|from|to|via|fill|stroke|outline|divide|caret|decoration|shadow)-(?:slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-(?:50|[1-9]00|950)\b/g;
const PALETTE_COLOR_MESSAGE =
  "Built-in palette color utility is not allowed: it paints a fixed color that " +
  "ignores the appearance settings and does not invert in dark mode. Use a " +
  "themable token: *-accent-{blue|purple|red|green|yellow}-N for accents or " +
  "*-default-N for neutrals.";

const noTauriImport = {
  create(context: RuleContext) {
    return {
      ImportDeclaration(node: ImportDeclarationNode) {
        const src = node.source.value;
        if (typeof src !== "string") return;
        if (src === "@tauri-apps" || src.startsWith("@tauri-apps/")) {
          context.report({ node, message: TAURI_MESSAGE });
        }
      },
    };
  },
};

const HOST_IMPORT_MESSAGE =
  "@loca/core-engine must stay runtime-agnostic. This module is host-specific " +
  "(native SQLite, Tauri, or Node). Reach it through the injected Host " +
  "(host.fs / host.openDb / host.secureStore / host.config) instead; the runtime " +
  "wiring belongs in @loca/core, not the engine.";
const DENO_GLOBAL_MESSAGE =
  "@loca/core-engine must stay runtime-agnostic: the Deno global is not " +
  "available in a webview. Reach files/env/time through the injected Host " +
  "(host.fs / host.config / host.now) instead. (Test files are exempt.)";

// A host-specific module the engine must never import directly. `@std/path`,
// `@std/encoding`, `@std/ulid`, `@std/assert` are pure and stay allowed; only
// `@std/fs` (the filesystem) is banned - it must go through host.fs.
function isBannedHostImport(src: string): boolean {
  const s = src.replace(/^jsr:/, "").replace(/^npm:/, "");
  return (
    s === "@db/sqlite" ||
    s.startsWith("@db/sqlite/") ||
    s === "@tauri-apps" ||
    s.startsWith("@tauri-apps/") ||
    s.startsWith("node:") ||
    s === "@std/fs" ||
    s.startsWith("@std/fs/")
  );
}

// Is `node` the Deno global reached via globalThis: `globalThis.Deno` or
// `globalThis["Deno"]`? (A bare `Deno.x` is handled separately by the object
// check.)
function isGlobalThisDeno(node: AstNode | null | undefined): boolean {
  if (!node || node.type !== "MemberExpression") return false;
  const o = node.object;
  if (!o || o.type !== "Identifier" || o.name !== "globalThis") return false;
  const p = node.property;
  return node.computed
    ? p?.type === "Literal" && p.value === "Deno"
    : p?.type === "Identifier" && p.name === "Deno";
}

// Keeps `@loca/core-engine` importable in a non-Deno (webview) runtime: bans
// host-coupled imports and every way of reaching the raw `Deno` global. Enabled
// only for the engine's non-test source via `overrides` in .oxlintrc.json.
// Coverage: `Deno.x` (object check), `globalThis.Deno`/`globalThis["Deno"]`
// (isGlobalThisDeno), and BINDING the global - `const d = Deno`,
// `const { readTextFile } = Deno`, `const d = globalThis.Deno` - via the
// VariableDeclarator check, so destructuring can't smuggle it past the member
// check. If the plugin host doesn't surface a node it simply no-ops and the
// import ban still holds.
const noHostImport = {
  create(context: RuleContext) {
    return {
      ImportDeclaration(node: ImportDeclarationNode) {
        const src = node.source.value;
        if (typeof src === "string" && isBannedHostImport(src)) {
          context.report({ node, message: HOST_IMPORT_MESSAGE });
        }
      },
      MemberExpression(node: MemberExpressionNode) {
        const obj = node.object;
        if (obj && obj.type === "Identifier" && obj.name === "Deno") {
          context.report({ node, message: DENO_GLOBAL_MESSAGE });
          return;
        }
        if (isGlobalThisDeno(node)) {
          context.report({ node, message: DENO_GLOBAL_MESSAGE });
        }
      },
      VariableDeclarator(node: VariableDeclaratorNode) {
        const init = node.init;
        if (!init) return;
        // Binding the Deno global to a name (incl. destructuring) - which the
        // member-access check above would not catch on its own.
        if ((init.type === "Identifier" && init.name === "Deno") || isGlobalThisDeno(init)) {
          context.report({ node, message: DENO_GLOBAL_MESSAGE });
        }
      },
    };
  },
};

const noEmDash = {
  create(context: RuleContext) {
    return {
      Program() {
        const lines = context.sourceCode.text.split("\n");
        for (let i = 0; i < lines.length; i++) {
          let col = lines[i].indexOf(EM_DASH);
          while (col !== -1) {
            context.report({
              message: EM_DASH_MESSAGE,
              loc: { line: i + 1, column: col },
            });
            col = lines[i].indexOf(EM_DASH, col + 1);
          }
        }
      },
    };
  },
};

const noBuiltinPaletteColor = {
  create(context: RuleContext) {
    return {
      Program() {
        const lines = context.sourceCode.text.split("\n");
        for (let i = 0; i < lines.length; i++) {
          PALETTE_COLOR_RE.lastIndex = 0;
          let m = PALETTE_COLOR_RE.exec(lines[i]);
          while (m !== null) {
            context.report({
              message: PALETTE_COLOR_MESSAGE,
              loc: { line: i + 1, column: m.index },
            });
            m = PALETTE_COLOR_RE.exec(lines[i]);
          }
        }
      },
    };
  },
};

export default {
  meta: { name: "loca" },
  rules: {
    "no-tauri-import": noTauriImport,
    "no-host-import": noHostImport,
    "no-em-dash": noEmDash,
    "no-builtin-palette-color": noBuiltinPaletteColor,
  },
};
