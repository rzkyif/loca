import type { SettingGroup } from "../types.ts";

export const snippetsGroup: SettingGroup = {
  id: "snippets",
  destination: "client-on-client",
  name: "Snippets",
  description:
    "Reusable text you trigger with #, @, or / while typing. A snippet can add to, replace, or insert into your message or the system prompt.",
  descriptionTier: "always",
  icon: "i-material-symbols-text-snippet-rounded",
  iconInactive: "i-material-symbols-text-snippet-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "snippets.list",
          name: "Snippets",
          type: "object_management",
          objectType: "snippets",
          defaultValue: "",
        },
      ],
    },
  ],
};
