import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ShareModalView from "../components/settings/ShareModalView.svelte";
import type { ShareTreeGroup } from "../components/settings/ShareTreeView.svelte";

// Scripted props for the share popup. The client computes every value (trees,
// export JSON, parse results, import label/counts) and owns the tab-slide and
// clipboard; these stand-ins exercise the export and import faces, including a
// pasted import whose rows would overwrite customized values.

const tabs = [
  { id: "import", label: "Import" },
  { id: "export", label: "Export" },
];

const exportTree: ShareTreeGroup[] = [
  {
    id: "appearance",
    name: "Appearance",
    fields: [{ id: "appearance.theme", name: "Theme", warn: false, disabled: false }],
    sections: [
      {
        key: "appearance-1",
        label: "Animations",
        fields: [
          { id: "appearance.animationsEnabled", name: "Enabled", warn: false, disabled: false },
        ],
      },
    ],
  },
];

const importTree: ShareTreeGroup[] = [
  {
    id: "appearance",
    name: "Appearance",
    fields: [
      { id: "appearance.theme", name: "Theme", warn: true, disabled: false },
      { id: "appearance.accentColor", name: "Accent color", warn: false, disabled: false },
    ],
    sections: [],
  },
];

const exportJsonText = JSON.stringify(
  { "appearance.theme": "dark", "appearance.animationsEnabled": true },
  null,
  2,
);

const importJsonText = JSON.stringify(
  { "appearance.theme": "light", "appearance.accentColor": "#3b82f6" },
  null,
  2,
);

export const shareModalSamples = {
  // Export face: a curated tree plus the generated JSON, ready to copy.
  export: {
    open: true,
    tabs,
    activeTab: "export",
    contentTab: "export",
    tabSlideMs: 0,
    animationsEnabled: false,
    exportTree,
    exportSelected: new Set(["appearance.theme", "appearance.animationsEnabled"]),
    exportJson: exportJsonText,
    exportHasOutput: true,
    copied: false,
    importText: "",
    parsedError: undefined,
    unknownKeysCount: 0,
    importError: null,
    importTree: [],
    importSelected: new Set<string>(),
    importHasTree: false,
    importCanApply: false,
    importing: false,
    importLabel: "Apply 0 Settings",
  },

  // Import face: pasted JSON with one overwrite warning row, mid-apply label.
  importOverwrite: {
    open: true,
    tabs,
    activeTab: "import",
    contentTab: "import",
    tabSlideMs: 0,
    animationsEnabled: false,
    exportTree,
    exportSelected: new Set<string>(),
    exportJson: "{}",
    exportHasOutput: false,
    copied: false,
    importText: importJsonText,
    parsedError: undefined,
    unknownKeysCount: 1,
    importError: null,
    importTree,
    importSelected: new Set(["appearance.theme", "appearance.accentColor"]),
    importHasTree: true,
    importCanApply: true,
    importing: false,
    importLabel: "Apply 1 Setting and Overwrite 1 Setting",
  },

  // Import face with a JSON parse error surfaced in the alert.
  importError: {
    open: true,
    tabs,
    activeTab: "import",
    contentTab: "import",
    tabSlideMs: 0,
    animationsEnabled: false,
    exportTree,
    exportSelected: new Set<string>(),
    exportJson: "{}",
    exportHasOutput: false,
    copied: false,
    importText: "{ not valid json",
    parsedError: "Not valid JSON",
    unknownKeysCount: 0,
    importError: null,
    importTree: [],
    importSelected: new Set<string>(),
    importHasTree: false,
    importCanApply: false,
    importing: false,
    importLabel: "Apply 0 Settings",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ShareModalView>>>;
