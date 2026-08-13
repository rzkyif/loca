import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type UserInputView from "../components/chat/UserInputView.svelte";

// Inert handlers for the scripted prompt states (permission / askUser): the
// gallery renders a frozen snapshot, so clicks do nothing.
const noop = (): void => {};

export const userInputSamples = {
  empty: { value: "", placeholder: "Enter your instructions..." },
  withText: {
    value: "How do I install Tomat on macOS?",
    placeholder: "Enter your instructions...",
  },
  voiceReady: {
    value: "",
    placeholder: "Enter your instructions...",
    showVoice: true,
    voiceTitle: "Hold to talk",
  },
  permission: {
    value: "",
    placeholder: "Enter your instructions...",
    permissionPrompt: {
      toolName: "shell",
      action: "run a program",
      detail: "git push origin main",
      declared: true,
    },
  },
  // askUser composer modes, one per question kind. The form replaces the
  // textarea (neutral) and its commit actions become the composer buttons:
  // single-select choice auto-submits from a tile (no buttons), diff hoists
  // Reject/Accept, image hoists its actions, files/table show a Submit.
  askChoice: {
    value: "",
    placeholder: "Enter your instructions...",
    askUserPrompt: {
      questions: [
        {
          question: "Which bowl should I fill?",
          options: [
            { label: "Kitchen", value: "kitchen", description: "the usual spot" },
            { label: "Patio", value: "patio" },
          ],
        },
      ],
      drafts: { 0: { text: "", picks: ["kitchen"], freestyleActive: false, rows: [] } },
      actions: [],
      autoFocus: false,
    },
  },
  askDiff: {
    value: "",
    placeholder: "Enter your instructions...",
    askUserPrompt: {
      questions: [
        {
          kind: "diff",
          question: "Apply this change?",
          title: "config.toml",
          before: "timeout = 30\nretries = 1\n",
          after: "timeout = 60\nretries = 3\n",
        },
      ],
      drafts: { 0: { text: "", picks: ["accept"], freestyleActive: false, rows: [] } },
      actions: [
        {
          icon: "i-material-symbols-close-rounded",
          label: "Reject",
          title: "Reject this change",
          onClick: noop,
        },
        {
          icon: "i-material-symbols-check-rounded",
          label: "Accept",
          title: "Accept this change",
          onClick: noop,
        },
      ],
      autoFocus: false,
    },
  },
  askFiles: {
    value: "",
    placeholder: "Enter your instructions...",
    askUserPrompt: {
      questions: [
        {
          kind: "files",
          question: "Which files should I read?",
          multiselect: true,
          entries: [
            { path: "src/main.ts", label: "main.ts", description: "entry point" },
            { path: "src/config.ts", label: "config.ts" },
            { path: "README.md", label: "README.md" },
          ],
        },
      ],
      drafts: {
        0: { text: "", picks: ["src/main.ts", "README.md"], freestyleActive: false, rows: [] },
      },
      actions: [
        {
          icon: "i-material-symbols-check-rounded",
          label: "Submit",
          title: "Submit your answer",
          onClick: noop,
        },
      ],
      autoFocus: false,
    },
  },
  askImage: {
    value: "",
    placeholder: "Enter your instructions...",
    askUserPrompt: {
      questions: [
        {
          kind: "image",
          question: "Does this look right?",
          mime: "image/png",
          dataB64:
            "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAIklEQVR42u3BAQ0AAADCoPdPbQ8HFAAAAAAAAAAAAAAA8G4wQAAB7OHIlwAAAABJRU5ErkJggg==",
          actions: [
            { label: "Looks good", value: "approve" },
            { label: "Try again", value: "retry" },
          ],
        },
      ],
      drafts: { 0: { text: "", picks: ["approve"], freestyleActive: false, rows: [] } },
      actions: [
        { label: "Looks good", title: "Looks good", onClick: noop },
        { label: "Try again", title: "Try again", onClick: noop },
      ],
      autoFocus: false,
    },
  },
  askTable: {
    value: "",
    placeholder: "Enter your instructions...",
    askUserPrompt: {
      questions: [
        {
          kind: "table",
          question: "Confirm the schedule:",
          columns: ["Day", "Task"],
          rows: [["Mon", "water plants"]],
        },
      ],
      drafts: {
        0: {
          text: "",
          picks: [],
          freestyleActive: false,
          rows: [
            ["Mon", "water plants"],
            ["Wed", "call vet"],
          ],
        },
      },
      actions: [
        {
          icon: "i-material-symbols-check-rounded",
          label: "Submit",
          title: "Submit your answer",
          onClick: noop,
        },
      ],
      autoFocus: false,
    },
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof UserInputView>>>;
