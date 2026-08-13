import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type MemoryDetailView from "../components/settings/MemoryDetailView.svelte";

export const memoryDetailSamples = {
  knowledge: {
    enabled: true,
    isSkill: false,
    editable: true,
    summaryStale: false,
    summary:
      "Brand and dependency conventions: write the brand as Tomat; prefer maintained packages.",
    draftTitle: "Project conventions",
    contentLoaded: true,
    draftContent:
      "Always write the brand as Tomat.\nPrefer maintained packages over hand-rolled code.",
  },
  skill: {
    enabled: true,
    isSkill: true,
    editable: true,
    summaryStale: false,
    summary: "Reads a whole thread and returns a short, structured recap.",
    draftTitle: "Summarize a thread",
    contentLoaded: true,
    draftDescription: "Turn a long thread into a short recap",
    draftContent: "# Summarize a thread\n\nRead every message, then return a 3-bullet recap.",
    suggestedTools: ["read_session", "search_messages"],
    availableTools: ["read_session", "search_messages", "list_sessions", "send_message"],
    files: ["recap.md", "checklist.md"],
    openFileName: "recap.md",
    openFileContent: "- who said what\n- decisions\n- open questions",
    openFileLoaded: true,
  },
  notSummarized: {
    enabled: true,
    isSkill: false,
    editable: true,
    summaryStale: true,
    draftTitle: "Travel preferences",
    contentLoaded: true,
    draftContent: "Window seat, no early flights, aisle on long-haul.",
  },
  readOnly: {
    enabled: false,
    isSkill: false,
    editable: false,
    summaryStale: false,
    summary: "Keep replies concise and concrete.",
    draftTitle: "Bundled tone guide",
    contentLoaded: true,
    draftContent: "Keep replies concise and concrete.",
  },
  titleError: {
    enabled: true,
    isSkill: false,
    editable: true,
    summaryStale: true,
    draftTitle: "",
    titleError: "Give this memory a title",
    contentLoaded: true,
    draftContent: "Always write the brand as Tomat.",
  },
  contentLoading: {
    enabled: true,
    isSkill: false,
    editable: true,
    summaryStale: true,
    draftTitle: "Project conventions",
    contentLoaded: false,
    draftContent: "",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof MemoryDetailView>>>;
