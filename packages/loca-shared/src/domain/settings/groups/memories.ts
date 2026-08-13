import type { SettingGroup } from "../types.ts";

// Memories: one group, two tabs. "Management" (default) is the full-height
// object_management manager; "Configuration" holds the relevant-memories
// options.
export const memoriesGroup: SettingGroup = {
  id: "memories",
  // Hybrid. The memories themselves are a shared store on the core (core); how
  // many to surface and whether to auto-apply skills is a per-client preference
  // the core applies per turn (client-on-core). The header shows "Client" +
  // "Core" chips.
  destination: ["client-on-core", "core"],
  name: "Memories",
  description:
    "Knowledge the agent can read and skills it can follow. Reference one while typing with its trigger.",
  descriptionTier: "always",
  icon: "i-material-symbols-description-rounded",
  iconInactive: "i-material-symbols-description-outline-rounded",
  tabs: [
    { id: "manage", label: "Management" },
    { id: "config", label: "Configuration" },
  ],
  sections: [
    {
      tab: "config",
      label: "Relevant Memories",
      destination: "client-on-core",
      fields: [
        {
          id: "memories.enabled",
          name: "Use Relevant Memories",
          description:
            "Let the agent see short summaries of memories related to your message, and read the full ones it needs.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
        {
          id: "memories.maxRelevant",
          name: "Max Relevant Memories",
          description: "How many memory summaries to include per message.",
          type: "number_slider",
          defaultValue: 3,
          min: 1,
          max: 10,
          step: 1,
          visibleWhen: { field: "memories.enabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "memories.skills.autoRelevancy",
          name: "Auto-Apply Relevant Skills",
          description:
            "Let the agent automatically apply skills relevant to your message. With this off, a skill applies only when you trigger it yourself. Knowledge is unaffected.",
          type: "boolean",
          defaultValue: true,
          visibleWhen: { field: "memories.enabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "memories.showEmptySelection",
          name: "Show Empty Selections",
          description:
            "Keep the memory-selection bubble in the chat even when no memories were found relevant to your message. Off hides it; turning it on also reveals past empty ones.",
          type: "boolean",
          defaultValue: false,
          visibleWhen: { field: "memories.enabled", eq: true },
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      tab: "manage",
      destination: "core",
      fields: [
        {
          id: "memories.list",
          name: "Memories",
          type: "object_management",
          objectType: "memories",
          defaultValue: "",
        },
      ],
    },
  ],
};
