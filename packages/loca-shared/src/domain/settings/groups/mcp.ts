import type { SettingGroup } from "../types.ts";

// MCP: connect Model Context Protocol servers as another provider. Their tools
// appear under Tools, their prompts as "/" commands, and their resources via
// "@". This group is just the server catalog (add / edit / remove / connect).
export const mcpGroup: SettingGroup = {
  id: "mcp",
  destination: "core",
  name: "MCP Servers",
  description:
    "Connect MCP (Model Context Protocol) servers as another source of tools, prompts, and resources. A server's tools appear under Tools; its prompts trigger with / and its resources with @.",
  descriptionTier: "always",
  icon: "i-material-symbols-lan-rounded",
  iconInactive: "i-material-symbols-lan-outline-rounded",
  sections: [
    {
      fields: [
        {
          id: "mcp.list",
          name: "MCP Servers",
          type: "object_management",
          objectType: "mcp",
          defaultValue: "",
        },
      ],
    },
  ],
};
