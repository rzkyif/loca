import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type StorageFieldView from "../components/settings/StorageFieldView.svelte";

// Plain display data for the storage browser. The client derives the equivalent
// shapes from a live `StorageTree` (formatting every byte size via its
// formatBytes helper and precomputing render flags); these are scripted
// stand-ins covering a populated tree (multiple categories, an expanded folder,
// a locked in-use item, a Clear action), an empty/zero state, and the loading
// and error states.

export const storageFieldSamples = {
  // A populated tree: Models (one locked, in-use file + a clearable one) is
  // expanded; Cache shows a Clear button; Sessions is collapsed.
  populated: {
    expanded: new Set(["__cat__:models", "__cat__:cache"]),
    selected: new Set(["models/Qwen2.5-1.5B-Instruct-Q4_K_M.gguf"]),
    tree: {
      totalSizeText: "11.4 GB",
      categories: [
        {
          id: "models",
          label: "Models",
          sizeText: "10.9 GB",
          canClear: true,
          clearIcon: "i-material-symbols-delete-outline-rounded",
          clearAriaLabel: "Clear Models",
          clearTitle: "Clear Models",
          settings: false,
          nodes: [
            {
              path: "models/Qwen2.5-7B-Instruct-Q4_K_M.gguf",
              name: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
              kind: "file",
              sizeText: "4.7 GB",
              locked: true,
              lockReason: "In use by the chat model",
              selectable: false,
              hasChildren: false,
              children: [],
            },
            {
              path: "models/Qwen2.5-1.5B-Instruct-Q4_K_M.gguf",
              name: "Qwen2.5-1.5B-Instruct-Q4_K_M.gguf",
              kind: "file",
              sizeText: "1.1 GB",
              locked: false,
              selectable: true,
              hasChildren: false,
              children: [],
            },
            {
              path: "models/embeddings",
              name: "embeddings",
              kind: "folder",
              sizeText: "5.1 GB",
              locked: false,
              selectable: true,
              hasChildren: true,
              children: [
                {
                  path: "models/embeddings/nomic-embed-text-v1.5.Q4_K_M.gguf",
                  name: "nomic-embed-text-v1.5.Q4_K_M.gguf",
                  kind: "file",
                  sizeText: "5.1 GB",
                  locked: false,
                  selectable: true,
                  hasChildren: false,
                  children: [],
                },
              ],
            },
          ],
        },
        {
          id: "sessions",
          label: "Sessions",
          sizeText: "84 MB",
          canClear: true,
          clearIcon: "i-material-symbols-delete-outline-rounded",
          clearAriaLabel: "Clear Sessions",
          clearTitle: "Clear Sessions",
          settings: false,
          nodes: [
            {
              path: "sessions/sessions.db",
              name: "sessions.db",
              kind: "file",
              sizeText: "84 MB",
              locked: false,
              selectable: true,
              hasChildren: false,
              children: [],
            },
          ],
        },
        {
          id: "cache",
          label: "Cache",
          sizeText: "412 MB",
          canClear: true,
          clearIcon: "i-material-symbols-delete-outline-rounded",
          clearAriaLabel: "Clear Cache",
          clearTitle: "Clear Cache",
          settings: false,
          nodes: [
            {
              path: "cache/embeddings",
              name: "embeddings",
              kind: "folder",
              sizeText: "412 MB",
              locked: false,
              selectable: true,
              hasChildren: false,
              children: [],
            },
          ],
        },
        {
          id: "settings",
          label: "Settings",
          sizeText: "12 KB",
          canClear: true,
          clearIcon: "i-material-symbols-restart-alt-rounded",
          clearAriaLabel: "Reset to defaults",
          clearTitle: "Reset to defaults",
          settings: true,
          nodes: [
            {
              path: "settings/settings.json",
              name: "settings.json",
              kind: "file",
              sizeText: "12 KB",
              locked: true,
              lockReason: "The live settings file",
              selectable: false,
              hasChildren: false,
              children: [],
            },
          ],
        },
      ],
    },
  },

  // A fresh install: every category present but empty, total zero, the expanded
  // Cache category showing its "Empty." placeholder.
  empty: {
    expanded: new Set(["__cat__:cache"]),
    selected: new Set<string>(),
    tree: {
      totalSizeText: "0 B",
      categories: [
        {
          id: "models",
          label: "Models",
          sizeText: "0 B",
          canClear: false,
          clearIcon: "i-material-symbols-delete-outline-rounded",
          clearAriaLabel: "Clear Models",
          clearTitle: "Clear Models",
          settings: false,
          nodes: [],
        },
        {
          id: "cache",
          label: "Cache",
          sizeText: "0 B",
          canClear: false,
          clearIcon: "i-material-symbols-delete-outline-rounded",
          clearAriaLabel: "Clear Cache",
          clearTitle: "Clear Cache",
          settings: false,
          nodes: [],
        },
      ],
    },
  },

  // First paint, before the provider responds.
  loading: {
    loading: true,
    tree: null,
  },

  // Provider failed (e.g. no paired Core).
  error: {
    loadError: "Could not reach the Core.",
    tree: null,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof StorageFieldView>>>;
