import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type { Extension } from "../../domain/extension.ts";
import type ExtensionDetailView from "../components/settings/ExtensionDetailView.svelte";

// A fully-populated installed extension; each sample below overrides the few
// fields that drive the View (status, undeclaredPolicy, tool counts).
const base: Extension = {
  id: "@loca/builtin",
  source: "npm",
  displayName: "Built-in Extension",
  description: "The reference toolset: files, shell, web fetch, and memory.",
  version: "1.4.0",
  installedPath: "~/.loca/stable/extensions/@loca__builtin",
  manifestHash: "sha256-manifest",
  contentHash: "sha256-content",
  status: "installed",
  hasDeps: true,
  hasDatabase: false,
  undeclaredPolicy: "ask",
  toolCount: 8,
  enabledToolCount: 6,
  installedAtMs: 1_717_000_000_000,
  updatedAtMs: 1_717_500_000_000,
};

// The detail body for one installed extension: the undeclared-permission policy
// toggle and the tool-count line, plus the drift warning when files changed.
// Covers an installed extension (with tools), a single-tool extension, a busy
// (in-flight policy change) state, and the drifted warning.
export const extensionDetailSamples = {
  installed: {
    extension: base,
  },
  singleTool: {
    extension: { ...base, toolCount: 1, enabledToolCount: 1, undeclaredPolicy: "deny" },
  },
  busy: {
    extension: base,
    busy: true,
  },
  drifted: {
    extension: { ...base, status: "drift", enabledToolCount: 0 },
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ExtensionDetailView>>>;
