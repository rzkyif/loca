import type { SettingGroup } from "../types.ts";

export const shortcutsGroup: SettingGroup = {
  id: "shortcuts",
  destination: "client-on-client",
  // Global keyboard shortcuts have no mobile equivalent (Android gives a
  // backgrounded app no foreground-hotkey access), so the group is hidden there.
  desktopOnly: true,
  name: "Shortcuts",
  description:
    "Keyboard shortcuts for common actions. Click a field, press the keys you want, and clear it to disable.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-keyboard-rounded",
  iconInactive: "i-material-symbols-keyboard-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "shortcuts.toggleWindow",
          name: "Show / Hide Window",
          description: "Bring the app to the front or hide it, from anywhere.",
          type: "shortcut",
          defaultValue: "super+ctrl+shift+z",
          optional: true,
          descriptionTier: "ondemand",
        },
        {
          id: "shortcuts.attachFile",
          name: "Attach File",
          description: "Attach a file to your next message. Only works in chat mode.",
          type: "shortcut",
          defaultValue: "super+ctrl+shift+a",
          optional: true,
          descriptionTier: "ondemand",
        },
        {
          id: "shortcuts.captureScreen",
          name: "Capture Screen",
          description: "Attach a screenshot of your whole screen. Only works in chat mode.",
          type: "shortcut",
          defaultValue: "super+ctrl+shift+s",
          optional: true,
          descriptionTier: "ondemand",
        },
        {
          id: "shortcuts.captureRegion",
          name: "Capture Screen Area",
          description: "Drag to select part of your screen and attach it. Only works in chat mode.",
          type: "shortcut",
          defaultValue: "super+ctrl+shift+x",
          optional: true,
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
