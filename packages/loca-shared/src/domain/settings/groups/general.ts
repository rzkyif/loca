import type { SettingGroup } from "../types.ts";

export const generalGroup: SettingGroup = {
  id: "general",
  destination: "client-on-client",
  name: "General",
  icon: "i-material-symbols-tune-rounded",
  iconInactive: "i-material-symbols-tune-rounded",
  sections: [
    {
      label: "Context",
      fields: [
        {
          id: "general.context.userName",
          name: "Your Name",
          description: "How the agent should address you.",
          type: "string",
          defaultValue: "",
          optional: true,
          placeholder: "e.g. Alex",
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.agentName",
          name: "Agent Name",
          description: "What the agent should call itself. Leave blank to let it choose.",
          type: "string",
          defaultValue: "",
          optional: true,
          placeholder: "e.g. Sebastian",
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.language",
          name: "Language",
          description: "The language the agent should reply in.",
          type: "string",
          defaultValue: "",
          optional: true,
          placeholder: "e.g. English",
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.locationAuto",
          name: "Detect My Location",
          description:
            "Guess your rough location from your system time zone, so you don't have to type it. Stays on your device.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.location",
          name: "Location",
          description: "A place the agent can take into account, like your city.",
          type: "string",
          defaultValue: "",
          optional: true,
          placeholder: "e.g. San Francisco, CA",
          visibleWhen: { field: "general.context.locationAuto", eq: false },
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.dateTime",
          name: "Share Date & Time",
          description: "Let the agent know the current date and time.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
        {
          id: "general.context.os",
          name: "Share Operating System",
          description: "Let the agent know which operating system you use.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Launch",
      // Start-at-login is a desktop-only concept; mobile apps have no autostart.
      desktopOnly: true,
      fields: [
        {
          id: "general.launch.autostart",
          name: "Start at Login",
          description: "Start Loca automatically when you log in to your computer.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Sessions",
      fields: [
        {
          id: "general.session.alwaysStartNew",
          name: "New Session on Launch",
          description: "Start with a blank session each time, instead of reopening your last one.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "general.session.storeSessions",
          name: "Save Sessions",
          description:
            "Keep your sessions and titles after the app closes. Off keeps them only until you quit.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
