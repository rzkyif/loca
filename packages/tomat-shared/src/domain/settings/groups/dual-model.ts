import type { SettingGroup } from "../types.ts";
import { externalModelSection } from "./factories.ts";

export const dualModelGroup: SettingGroup = {
  id: "dualModel",
  // Hybrid: whether to route complex messages is a per-client preference (the
  // core honors it per turn), but the secondary model's endpoint is a shared
  // backend on the core.
  destination: ["client-on-core", "core"],
  name: "Dual Model",
  description:
    "Keep simple messages on your fast default model and send only complex ones to a stronger model. Tomat decides per message; you never switch manually.",
  descriptionTier: "always",
  icon: "i-material-symbols-call-split-rounded",
  iconInactive: "i-material-symbols-call-split-rounded",
  sections: [
    {
      label: "General",
      destination: "client-on-core",
      fields: [
        {
          id: "dualModel.enabled",
          name: "Enable Dual Model",
          description:
            "Route complex messages to a stronger secondary model automatically. The secondary model answers directly, without thinking, for a fast reply. The rule that decides this lives in Prompts > Complexity Check.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
      ],
    },
    externalModelSection({
      idPrefix: "dualModel.external",
      label: "Secondary Model",
      visibleWhen: { field: "dualModel.enabled", eq: true },
      baseUrlDescription:
        "The secondary model's API endpoint. Must be HTTPS (HTTP allowed only for localhost).",
      apiKeyDescription: "API key for the secondary model. Stored securely in your keychain.",
      model: { description: "The secondary model name, e.g. gpt-4o.", placeholder: "gpt-4o" },
      contextSize: true,
    }),
  ],
};
