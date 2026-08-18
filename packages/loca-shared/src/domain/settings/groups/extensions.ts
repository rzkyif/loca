import type { SettingGroup } from "../types.ts";

// Extensions: install and remove the packages that provide tools and memories.
// Per-tool enabling and permissions live under Tools; this group is just the
// provider catalog (install / update / uninstall / delete).
export const extensionsGroup: SettingGroup = {
  id: "extensions",
  destination: "core",
  // Hidden when the connected Core cannot run a tool worker. Installing is not
  // the question: an extension installs fine with no worker spawner and then
  // every one of its tools fails at call time, which is the state worth hiding.
  // A property of the Core, so it is a capability rather than `desktopOnly`: a
  // phone paired to a desktop Core still manages that Core's extensions.
  requiresCapability: "extensions",
  name: "Extensions",
  description:
    "Install extensions to add tools, knowledge, and skills; the built-in extension provides the starter set. Turn each tool on under Tools and each memory on under Memories.",
  descriptionTier: "always",
  icon: "i-material-symbols-extension-rounded",
  iconInactive: "i-material-symbols-extension-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "extensions.list",
          name: "Extensions",
          type: "object_management",
          objectType: "extensions",
          defaultValue: "",
        },
      ],
    },
    {
      fields: [
        {
          id: "extensions.ignorePostinstallScripts",
          name: "Block Install Scripts",
          description:
            "Stop extension dependencies from running setup scripts during install, a common malware path. Leave on unless an extension must build native code.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
