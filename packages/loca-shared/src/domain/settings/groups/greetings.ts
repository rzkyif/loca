import type { SettingGroup } from "../types.ts";
import { DEFAULT_GREETING_INSTRUCTION } from "../../prompts.ts";

export const greetingsGroup: SettingGroup = {
  id: "greetings",
  destination: "client-on-client",
  name: "Greetings",
  description: "Have the agent open a session to greet you when the app starts.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-waving-hand-rounded",
  iconInactive: "i-material-symbols-waving-hand-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "greetings.enabled",
          name: "Enable Greetings",
          description: "",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "none",
        },
        {
          id: "greetings.runOn",
          name: "When to Greet",
          description:
            "Greet only when Loca opens itself at login, on every launch, or every time the window is shown.",
          type: "select",
          defaultValue: "autostart",
          options: [
            { value: "autostart", label: "Automatic Start Only" },
            { value: "every_start", label: "Every Start" },
            { value: "every_show", label: "Every Show" },
          ],
          // Login autostart has no mobile equivalent, so the choice is hidden
          // there; a mobile launch always greets (it reports as an automatic
          // start), matching "every start". "Every Show" also greets each time
          // the window is revealed from hidden, which mobile has no equivalent of.
          desktopOnly: true,
          visibleWhen: { field: "greetings.enabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "greetings.showCooldown",
          name: "Greeting Cooldown",
          description: "How long to wait before another greeting can run.",
          type: "number",
          defaultValue: 10,
          suffix: "seconds",
          regex: [
            {
              regex: "^([5-9]|[1-9][0-9]|[1-9][0-9]{2}|[12][0-9]{3}|3[0-5][0-9]{2}|3600)$",
              errorMessage: "Must be 5-3600",
            },
          ],
          visibleWhen: { field: "greetings.enabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "greetings.sessionTitle",
          name: "Session Title",
          description:
            'Title for the session a greeting opens. Use {datetime} to insert the start time, e.g. "Greeting 2026-06-15 14:30".',
          type: "string",
          defaultValue: "Greeting {datetime}",
          visibleWhen: { field: "greetings.enabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "greetings.instruction",
          name: "Greeting Prompt",
          description:
            "The prompt sent when a greeting runs. Write it as a message from you to the agent.",
          type: "multiline",
          defaultValue: DEFAULT_GREETING_INSTRUCTION,
          visibleWhen: { field: "greetings.enabled", eq: true },
          descriptionTier: "ondemand",
        },
      ],
    },
  ],
};
