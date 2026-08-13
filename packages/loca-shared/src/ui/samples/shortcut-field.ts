import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ShortcutFieldView from "../components/settings/ShortcutFieldView.svelte";

// Plain display data for the keyboard-shortcut capture row. The client owns the
// live setting value, the key-capture logic, and the editable/error flags; it
// splits the stored accelerator into key parts and feeds them here. These cover
// a set shortcut (with the clear button), an empty/unset row, and the recording
// state.

export const shortcutFieldSamples = {
  set: {
    fieldName: "Toggle window",
    segments: ["ctrl", "alt", "k"],
    showClear: true,
  },
  empty: {
    fieldName: "Toggle window",
    segments: [],
  },
  recording: {
    fieldName: "Toggle window",
    segments: ["ctrl", "alt", "k"],
    recording: true,
    showClear: true,
  },
  error: {
    fieldName: "Toggle window",
    segments: ["ctrl", "alt", "k"],
    hasError: true,
    showClear: true,
  },
  readOnly: {
    fieldName: "Toggle window",
    segments: ["ctrl", "alt", "k"],
    editable: false,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ShortcutFieldView>>>;
