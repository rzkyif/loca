import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type RelevantToolsView from "../components/chat/messages/RelevantToolsView.svelte";

export const relevantToolsSamples = {
  twoPhase: {
    defaultExpanded: true,
    phase1: [
      { toolId: "fs.read", name: "read_file", description: "", score: 0.71 },
      { toolId: "fs.write", name: "write_file", description: "", score: 0.55 },
    ],
    phase2: [{ toolId: "fs.read", name: "read_file", description: "" }],
    alwaysAvailable: [
      {
        toolId: "sys.now",
        name: "current_time",
        description: "",
      },
    ],
    mcp: [{ toolId: "gh.search", name: "github_search", description: "" }],
  },
  named: {
    defaultExpanded: true,
    // The user named a tool directly ("use the write_memory tool"); nothing
    // scored, so it rides in via the Named Tools section, not phase 1/2.
    phase1: [],
    phase2: [],
    nameMatched: [{ toolId: "memory.write", name: "write_memory", description: "" }],
  },
  collapsed: {
    defaultExpanded: false,
    phase1: [{ toolId: "fs.read", name: "read_file", description: "", score: 0.71 }],
  },
  emptyPhase: {
    defaultExpanded: true,
    phase1: [{ toolId: "fs.read", name: "read_file", description: "", score: 0.71 }],
    phase2: [],
  },
  error: {
    defaultExpanded: true,
    status: "error",
    errorMessage: "Embedding model timed out after 30s.",
    phase1: [{ toolId: "fs.read", name: "read_file", description: "", score: 0.71 }],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof RelevantToolsView>>>;
