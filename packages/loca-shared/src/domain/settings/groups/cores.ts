import type { SettingGroup } from "../types.ts";

// Paired-cores registry. Backed by client settings.json (the {id, name,
// baseUrl, addedAtMs} triples plus a `currentCoreId` pointer) + OS keychain
// (the bearer tokens). The list itself is managed by
// `packages/loca-client/src/ui/lib/core/cores.ts` via the platform's
// clientSettings.read/write, not by the settingsState save loop. This
// schema entry exists so the per-key destination map in settingsState
// knows the `cores.list` key belongs to the client destination, and so
// the Settings UI has a stable group id for the cores manager.
export const coresGroup: SettingGroup = {
  id: "cores",
  destination: "client-on-client",
  name: "Cores",
  description:
    "The cores this app is paired with. Tokens stay in your device keychain; add or remove cores through pairing.",
  descriptionTier: "always",
  icon: "i-material-symbols-hub",
  iconInactive: "i-material-symbols-hub-outline",
  sections: [
    {
      fields: [
        {
          id: "cores.list",
          name: "Cores",
          type: "object_management",
          objectType: "cores",
          defaultValue: "",
        },
      ],
    },
  ],
};
