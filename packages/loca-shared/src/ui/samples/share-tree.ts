import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ShareTreeView from "../components/settings/ShareTreeView.svelte";

// Plain tree data for the share selector. The client derives the equivalent
// shapes from the live settings schema; these are scripted stand-ins covering
// the tri-state checkbox cases (checked / unchecked / indeterminate), nested
// group -> section -> field levels, and the import overwrite warning rows.

export const shareTreeSamples = {
  // A group whose direct fields are all selected (parent "all", checked).
  allSelected: {
    nodes: [
      {
        id: "appearance",
        name: "Appearance",
        fields: [
          { id: "appearance.theme", name: "Theme", warn: false, disabled: false },
          { id: "appearance.accentColor", name: "Accent color", warn: false, disabled: false },
        ],
        sections: [],
      },
    ],
    selected: new Set(["appearance.theme", "appearance.accentColor"]),
  },

  // Same group, none selected (parent "none", unchecked).
  noneSelected: {
    nodes: [
      {
        id: "appearance",
        name: "Appearance",
        fields: [
          { id: "appearance.theme", name: "Theme", warn: false, disabled: false },
          { id: "appearance.accentColor", name: "Accent color", warn: false, disabled: false },
        ],
        sections: [],
      },
    ],
    selected: new Set<string>(),
  },

  // Some-but-not-all selected (parent "some", indeterminate).
  indeterminate: {
    nodes: [
      {
        id: "appearance",
        name: "Appearance",
        fields: [
          { id: "appearance.theme", name: "Theme", warn: false, disabled: false },
          { id: "appearance.accentColor", name: "Accent color", warn: false, disabled: false },
        ],
        sections: [],
      },
    ],
    selected: new Set(["appearance.theme"]),
  },

  // Nested group -> section -> field across multiple groups, mixed selection.
  nested: {
    nodes: [
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
              { id: "appearance.animationsSpeed", name: "Speed", warn: false, disabled: false },
            ],
          },
        ],
      },
      {
        id: "models",
        name: "Models",
        fields: [],
        sections: [
          {
            key: "models-0",
            label: "Chat",
            fields: [
              { id: "models.chat.temperature", name: "Temperature", warn: false, disabled: false },
              { id: "models.chat.contextSize", name: "Context size", warn: false, disabled: false },
            ],
          },
        ],
      },
    ],
    selected: new Set(["appearance.theme", "models.chat.temperature"]),
  },

  // Import tree with overwrite warnings (warn rows) and a no-op disabled field.
  withWarnings: {
    nodes: [
      {
        id: "appearance",
        name: "Appearance",
        fields: [
          { id: "appearance.theme", name: "Theme", warn: true, disabled: false },
          { id: "appearance.accentColor", name: "Accent color", warn: false, disabled: false },
          { id: "appearance.fontScale", name: "Font scale", warn: false, disabled: true },
        ],
        sections: [
          {
            key: "appearance-1",
            label: "Animations",
            fields: [
              { id: "appearance.animationsEnabled", name: "Enabled", warn: true, disabled: false },
            ],
          },
        ],
      },
    ],
    selected: new Set([
      "appearance.theme",
      "appearance.accentColor",
      "appearance.animationsEnabled",
    ]),
  },

  // A group every field of which is a no-op (parent shown disabled).
  allDisabled: {
    nodes: [
      {
        id: "models",
        name: "Models",
        fields: [
          { id: "models.chat.temperature", name: "Temperature", warn: false, disabled: true },
          { id: "models.chat.contextSize", name: "Context size", warn: false, disabled: true },
        ],
        sections: [],
      },
    ],
    selected: new Set<string>(),
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ShareTreeView>>>;
