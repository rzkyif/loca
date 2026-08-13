import type { SettingGroup } from "../types.ts";
import {
  ASSISTANT_PROMPT,
  DEFAULT_AUTOCORRECT_PROMPT,
  DEFAULT_COMPLEXITY_DETECTION_PROMPT,
  DEFAULT_CONTEXT_TEMPLATE,
  DEFAULT_MEMORY_SUMMARY_PROMPT,
  DEFAULT_MERGE_TRANSCRIPTION_PROMPT,
  DEFAULT_TITLE_GENERATION_PROMPT,
  TOOL_ONLY_PROMPT,
} from "../../prompts.ts";

export const promptsGroup: SettingGroup = {
  id: "prompts",
  // The prompts a user gives the agent are personal: each client keeps its own.
  // They drive the core per turn and for automated sessions, so the whole group
  // lives in the per-client core overlay (client-on-core), which renders as the
  // "Client" badge.
  destination: "client-on-core",
  name: "Prompts",
  description: "Customize the instructions and helper prompts Loca sends behind the scenes.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-chat-bubble-rounded",
  iconInactive: "i-material-symbols-chat-bubble-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "prompts.showSystemPrompt",
          name: "Show System Prompt",
          description:
            "Show the active system prompt as a bubble at the top of each session.\nThe bubble updates to reflect any snippet-triggered changes.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Default System Prompt",
      fields: [
        {
          id: "prompts.defaultSystemPrompt.preset",
          name: "Preset",
          description:
            "A starting system prompt for every chat. Editing the text below switches this to Custom.",
          type: "preset",
          defaultValue: "disabled",
          presetConfig: {
            options: [
              {
                id: "disabled",
                label: "None",
                title: "None",
                description: "No system prompt is sent. The model runs with its default behavior.",
                defaults: { "prompts.defaultSystemPrompt": "" },
              },
              {
                id: "tool-only",
                label: "Tool Only",
                title: "Tool Only",
                description:
                  "Forces short, cautious replies and a strong preference for tool use. Suited to small on-device models that cannot reason reliably on their own.",
                defaults: { "prompts.defaultSystemPrompt": TOOL_ONLY_PROMPT },
              },
              {
                id: "assistant",
                label: "Assistant",
                title: "Assistant",
                description:
                  "A professional, capable general-purpose assistant that answers directly and uses tools when helpful.",
                defaults: { "prompts.defaultSystemPrompt": ASSISTANT_PROMPT },
              },
            ],
            secondaryOptions: [
              {
                id: "custom",
                label: "Custom",
                title: "Custom",
                description:
                  "Write your own system prompt. Editing the prompt manually switches to this option automatically.",
              },
            ],
          },
        },
        {
          id: "prompts.defaultSystemPrompt",
          name: "System Prompt",
          description: "Instructions sent to the agent at the start of every chat.",
          type: "multiline",
          defaultValue: "",
          optional: true,
          visibleWhen: {
            field: "prompts.defaultSystemPrompt.preset",
            eq: "custom",
          },
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Context Template",
      defaultCollapsed: true,
      fields: [
        {
          id: "prompts.contextTemplate",
          name: "Template",
          description:
            "Added to the system prompt to share your enabled context (name, language, location, date, OS). `{name}` inserts a value; `[name:text]` drops out when that value is unset.",
          type: "multiline",
          defaultValue: DEFAULT_CONTEXT_TEMPLATE,
          regex: [{ regex: "\\S", errorMessage: "Template cannot be empty" }],
          mono: true,
          descriptionTier: "always",
        },
      ],
    },
    {
      label: "Helper Prompts",
      defaultCollapsed: true,
      fields: [
        {
          id: "prompts.titleGenerationPrompt",
          name: "Title Generation",
          description:
            "Creates a short session title from the conversation. `{firstMessage}` inserts the opening message and `{recentMessages}` inserts the latest messages that fit the context window.",
          type: "multiline",
          defaultValue: DEFAULT_TITLE_GENERATION_PROMPT,
          regex: [{ regex: "\\S", errorMessage: "Prompt cannot be empty" }],
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.titleGenerationThinkingBudget",
          name: "Title Thinking Budget",
          description: "Tokens title generation may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 128,
          min: 0, // a token budget can't be negative
          placeholder: "0",
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.autocorrectPrompt",
          name: "Transcription Cleanup",
          description: "Cleans up voice transcriptions before they reach the input.",
          type: "multiline",
          defaultValue: DEFAULT_AUTOCORRECT_PROMPT,
          regex: [{ regex: "\\S", errorMessage: "Prompt cannot be empty" }],
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.autocorrectThinkingBudget",
          name: "Cleanup Thinking Budget",
          description: "Tokens transcription cleanup may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 0,
          min: 0, // a token budget can't be negative
          placeholder: "0",
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.memorySummaryPrompt",
          name: "Memory Summary",
          description: "Creates the short summary used to match and surface each relevant memory.",
          type: "multiline",
          defaultValue: DEFAULT_MEMORY_SUMMARY_PROMPT,
          regex: [{ regex: "\\S", errorMessage: "Prompt cannot be empty" }],
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.memorySummaryThinkingBudget",
          name: "Summary Thinking Budget",
          description: "Tokens the memory summary may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 256,
          min: 0, // a token budget can't be negative
          placeholder: "0",
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.mergeTranscriptionPrompt",
          name: "Transcription Merge",
          description: "Merges new dictation into text already in the input.",
          type: "multiline",
          defaultValue: DEFAULT_MERGE_TRANSCRIPTION_PROMPT,
          regex: [{ regex: "\\S", errorMessage: "Prompt cannot be empty" }],
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.mergeTranscriptionThinkingBudget",
          name: "Merge Thinking Budget",
          description: "Tokens transcription merge may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 0,
          placeholder: "0",
          descriptionTier: "ondemand",
        },
        {
          id: "prompts.complexityDetectionPrompt",
          name: "Complexity Check",
          description:
            'Dual Model uses this to sort each message. If the reply says "complex" the message goes to your secondary model; otherwise it stays on the default.',
          type: "multiline",
          defaultValue: DEFAULT_COMPLEXITY_DETECTION_PROMPT,
          regex: [{ regex: "\\S", errorMessage: "Prompt cannot be empty" }],
          descriptionTier: "always",
        },
        {
          id: "prompts.complexityDetectionThinkingBudget",
          name: "Complexity Thinking Budget",
          description: "Tokens the complexity check may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 0,
          placeholder: "0",
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
