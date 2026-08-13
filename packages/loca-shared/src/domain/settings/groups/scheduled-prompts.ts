import type { SettingGroup } from "../types.ts";

export const scheduledPromptsGroup: SettingGroup = {
  id: "scheduledPrompts",
  destination: "core",
  name: "Scheduled Prompts",
  description:
    "Prompts that run on their own on a schedule. Each run opens a new session, and the agent can propose one in chat for you to approve.",
  descriptionTier: "always",
  icon: "i-material-symbols-schedule-rounded",
  iconInactive: "i-material-symbols-schedule-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "scheduledPrompts.list",
          name: "Scheduled Prompts",
          type: "object_management",
          objectType: "scheduled_prompts",
          defaultValue: "",
        },
      ],
    },
  ],
};
