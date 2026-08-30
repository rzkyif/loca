// Local oxlint plugin housing Loca's custom lint rules. Registered via
// `jsPlugins` in the root `.oxlintrc.json` under the `loca/` namespace.
//
// Rules:
//   - no-tauri-import: forbid direct `@tauri-apps/*` imports; the client must
//     reach Tauri through `$lib/platform/`. Scoped to the client UI tree via
//     `overrides`, with `platform/tauri.ts` exempted.
//     `check-tauri-imports-svelte.ts` covers the `.svelte` gap oxlint cannot
//     parse.
//   - no-em-dash: forbid the em dash (U+2014) anywhere. Scans raw source, so
//     every occurrence is caught, not just AST string nodes.
//   - no-builtin-palette-color: forbid the built-in UnoCSS/Tailwind palette color
//     utilities (a property prefix + a palette hue + a numeric shade). Those
//     paint a fixed sRGB color that ignores the appearance settings and does NOT
//     theme-invert, where the themable tokens resolve per-scope CSS variables and
//     carry inversion for free. Also raw-source, so it catches class strings;
//     check-builtin-palette-color.ts covers the file types oxlint cannot parse.
//
// Keep this file em-dash-free, or the rule will flag its own source.

interface ImportDeclarationNode {
  source: { value: unknown };
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
// hue, and a numeric shade. The `*-accent-*` / `*-default-*` tokens never match:
// their hue segment is literally `accent`/`default`. KEEP IN SYNC with the copy
// in check-builtin-palette-color.ts.
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
    "no-em-dash": noEmDash,
    "no-builtin-palette-color": noBuiltinPaletteColor,
  },
};
