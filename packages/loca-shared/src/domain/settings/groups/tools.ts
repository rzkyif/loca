import type { SettingGroup } from "../types.ts";

// Tools: one group with two tabs. "Management" (default) lists every tool from
// every provider (extensions and MCP servers) so you can enable/disable each
// and manage its permissions in one place; "Configuration" holds the
// tool-calling runtime settings. Tool management is intentionally NOT gated
// behind `tools.enabled`: you can configure tools before turning tool use on.
export const toolsGroup: SettingGroup = {
  id: "tools",
  // Hybrid. Whether tools are on and how they're selected per message is a
  // per-client preference the core applies per turn (client-on-core); the tool
  // worker pool and the installed-tool catalog are shared core resources
  // (core); the risky-grant "do not show again" flag is a local UI flag
  // (client-on-client). The header collapses to "Client" + "Core" chips.
  destination: ["client-on-core", "core", "client-on-client"],
  name: "Tools",
  description:
    "Tools the agent can use, like web search or file access. Turn individual tools on and set their permissions under Management; turn tool use on and tune selection under Configuration. They come from Extensions and MCP servers.",
  descriptionTier: "always",
  icon: "i-material-symbols-build-rounded",
  iconInactive: "i-material-symbols-build-outline-rounded",
  tabs: [
    { id: "manage", label: "Management" },
    { id: "config", label: "Configuration" },
  ],
  sections: [
    {
      tab: "config",
      label: "General",
      destination: "client-on-core",
      fields: [
        {
          id: "tools.enabled",
          name: "Enable Tools",
          description: "Let the agent call your enabled tools while chatting.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      tab: "config",
      label: "Tool Selection",
      destination: "client-on-core",
      defaultCollapsed: true,
      visibleWhen: { field: "tools.enabled", eq: true },
      fields: [
        {
          id: "tools.filteringEnabled",
          name: "Select Relevant Tools",
          description:
            "Before each message, offer the model only the tools likely to help instead of all of them. Keeps things fast and focused when you have many tools.",
          type: "boolean",
          defaultValue: true,
          descriptionTier: "ondemand",
        },
        {
          id: "tools.filteringMinTools",
          name: "Skip When Few Tools",
          description:
            "When you have fewer than this many tools, skip selection and offer them all. 0 always selects.",
          type: "number",
          defaultValue: 0,
          regex: [
            {
              regex: "^([0-9]|[1-9][0-9]{1,2}|1000)$",
              errorMessage: "Must be 0-1000",
            },
          ],
          visibleWhen: { field: "tools.filteringEnabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "tools.alwaysAvailableEnabled",
          name: "Allow Always-Available Tools",
          description:
            "Let some tools be offered on every message, skipping relevance selection. Choose which in each tool's own settings; a tool starts on if its provider marks it always available.",
          type: "boolean",
          defaultValue: true,
          visibleWhen: { field: "tools.filteringEnabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "tools.maxTools",
          name: "Max Tools Per Message",
          description: "The most tools to offer the model at once.",
          type: "number",
          defaultValue: 30,
          regex: [{ regex: "^[1-9][0-9]?$", errorMessage: "Must be 1-99" }],
          visibleWhen: { field: "tools.filteringEnabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "tools.secondPassEnabled",
          name: "Refine Selection With AI",
          description:
            "After the quick relevance match, have the model review the shortlist and drop tools that don't fit. More accurate, a little slower.",
          type: "boolean",
          defaultValue: true,
          visibleWhen: { field: "tools.filteringEnabled", eq: true },
          descriptionTier: "ondemand",
        },
        {
          id: "tools.filterThinkingBudget",
          name: "Refinement Thinking Budget",
          description: "Tokens the AI refinement step may spend thinking. 0 turns thinking off.",
          type: "number",
          defaultValue: 0,
          placeholder: "0",
          visibleWhen: {
            allOf: [
              { field: "tools.filteringEnabled", eq: true },
              { field: "tools.secondPassEnabled", eq: true },
            ],
          },
          descriptionTier: "ondemand",
        },
        {
          id: "tools.maxHops",
          name: "Max Tool Rounds Per Message",
          description: "Cap on back-to-back tool calls in one message, to prevent runaway loops.",
          type: "number",
          defaultValue: 5,
          regex: [{ regex: "^[1-9][0-9]?$", errorMessage: "Must be 1-99" }],
          descriptionTier: "ondemand",
        },
        {
          id: "tools.showEmptySelection",
          name: "Show Empty Selections",
          description:
            "Keep the tool-selection bubble in the chat even when no tools were found relevant to your message. Off hides it; turning it on also reveals past empty ones.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      tab: "config",
      label: "Tool Execution",
      destination: "core",
      defaultCollapsed: true,
      visibleWhen: { field: "tools.enabled", eq: true },
      fields: [
        {
          id: "extensions.maxWarmWorkers",
          name: "Tools Kept Ready",
          description:
            "How many tools to keep loaded for instant reuse. Higher cuts delay; lower saves RAM.",
          type: "number",
          defaultValue: 8,
          regex: [{ regex: "^[1-9][0-9]?$", errorMessage: "Must be 1-99" }],
          descriptionTier: "ondemand",
        },
        {
          id: "extensions.workerIdleMs",
          name: "Unload Tools After",
          description: "Unload a tool from RAM after this long unused.",
          type: "number",
          defaultValue: 300000,
          suffix: "ms",
          regex: [
            {
              regex: "^[0-9]+$",
              errorMessage: "Must be a non-negative integer",
            },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "extensions.callTimeoutMs",
          name: "Tool Timeout",
          description:
            "Stop a tool if a single call runs longer than this. Waiting on you doesn't count. 0 disables.",
          type: "number",
          defaultValue: 60000,
          suffix: "ms",
          regex: [
            {
              regex: "^[0-9]+$",
              errorMessage: "Must be a non-negative integer",
            },
          ],
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      tab: "manage",
      destination: "core",
      fields: [
        {
          id: "tools.list",
          name: "Tools",
          type: "object_management",
          objectType: "tools",
          defaultValue: "",
        },
      ],
    },
    {
      tab: "manage",
      destination: "client-on-client",
      fields: [
        // Hidden persisted flag: set from the risky-permission confirm dialog's
        // "do not show again" checkbox in the tool detail view. Never rendered.
        {
          id: "extensions.skipRiskyGrantWarning",
          name: "Skip Risky Permission Warning",
          description: "",
          type: "boolean",
          defaultValue: false,
          visibleWhen: { field: "tools.list", eq: "__never__" },
          descriptionTier: "none",
        },
        // Hidden persisted flag: set from the "Don't Ask Again" choice in the
        // prompt offering to install the built-in tools when you turn tools on.
        // Suppresses that prompt thereafter. Never rendered.
        {
          id: "tools.builtinPromptDismissed",
          name: "Built-in Tools Prompt Dismissed",
          description: "",
          type: "boolean",
          defaultValue: false,
          visibleWhen: { field: "tools.list", eq: "__never__" },
          descriptionTier: "none",
        },
      ],
    },
  ],
};
