import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SnippetDetailView from "../components/settings/SnippetDetailView.svelte";

// The client derives these from a live snippet draft: it validates the name and
// builds the symbol options whose labels are the full trigger preview (`symbol` +
// name), marking the recommended one. These scripted stand-ins cover a
// fully-filled snippet, an empty/new one, and a name-collision error state.

// Each option label is the live trigger preview for the draft name, so the
// dropdown shows `@name` / `#name` / `/name` rather than bare symbols.
const symbolOptions = (name: string) => {
  const word = name || "name";
  return [
    { value: "#", label: `#${word}` },
    { value: "@", label: `@${word}` },
    { value: "/", label: `/${word}  (Recommended)` },
  ];
};

const placementOptions = [
  { value: "prepend-system", label: "Prepend System Prompt" },
  { value: "replace-system", label: "Replace System Prompt" },
  { value: "append-system", label: "Append System Prompt" },
  { value: "prepend-user", label: "Prepend User Prompt" },
  { value: "replace-user", label: "Replace User Prompt" },
  { value: "insert-user", label: "Insert in User Prompt" },
  { value: "append-user", label: "Append User Prompt" },
];

export const snippetDetailSamples = {
  // A saved command snippet: `/` symbol recommended for whole-prompt placement.
  filled: {
    draftName: "summarize",
    draftSymbol: "/",
    draftPlacement: "replace-user",
    draftText: "Summarize the conversation so far in three short bullets.",
    symbolOptions: symbolOptions("summarize"),
    placementOptions,
  },
  // A fresh snippet before anything is typed: empty name, the option previews
  // fall back to the placeholder word.
  empty: {
    draftName: "",
    draftSymbol: "#",
    draftPlacement: "insert-user",
    draftText: "",
    symbolOptions: symbolOptions(""),
    placementOptions,
  },
  // The chosen trigger collides with another snippet's trigger.
  error: {
    draftName: "scientist",
    draftSymbol: "@",
    draftPlacement: "prepend-system",
    draftText: "You are a meticulous research scientist.",
    nameError: "This trigger is already used by another snippet",
    symbolOptions: symbolOptions("scientist"),
    placementOptions,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SnippetDetailView>>>;
