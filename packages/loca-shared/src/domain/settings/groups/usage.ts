import type { SettingGroup } from "../types.ts";

// The Usage group spans both destinations: it shows live resource usage and
// on-disk storage for the local client AND the paired core, split into two
// collapsible sections. All fields are render-only (no persisted values).
export const usageGroup: SettingGroup = {
  id: "usage",
  destination: ["client-on-client", "core"],
  name: "Usage",
  description: "What Loca is using right now: RAM, CPU, and disk on this device and on the Core.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-analytics-rounded",
  iconInactive: "i-material-symbols-analytics-outline-rounded",
  sections: [
    {
      label: "On This Device",
      destination: "client-on-client",
      fields: [
        {
          id: "usage.clientServices",
          name: "Services",
          description: "Live RAM and CPU use of the app.",
          type: "services",
          scope: "client",
          defaultValue: "",
          descriptionTier: "ondemand",
        },
        {
          id: "usage.clientStorage",
          name: "Storage",
          description:
            "App data on this device, like settings and logs. Clear logs or reset settings here.",
          type: "storage",
          scope: "client",
          defaultValue: "",
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "On the Core",
      destination: "core",
      fields: [
        {
          id: "usage.coreServices",
          name: "Services",
          description: "Live RAM and CPU use of the Core and its helpers.",
          type: "services",
          scope: "core",
          defaultValue: "",
          descriptionTier: "ondemand",
        },
        {
          id: "usage.coreStorage",
          name: "Storage",
          description:
            "Everything the core stores on disk, by type. Select items to delete, or clear a whole category.",
          type: "storage",
          scope: "core",
          defaultValue: "",
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
