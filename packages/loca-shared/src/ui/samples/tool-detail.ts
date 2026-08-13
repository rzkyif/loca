import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ToolDetailView from "../components/settings/ToolDetailView.svelte";

// One tool's detail body: the enable toggle and its pre-formatted grant rows.
// Covers an enabled extension tool with a mix of required/optional permissions,
// a disabled tool, an enabled tool whose required permission is denied (the
// caveat note), and an MCP tool that carries no local permissions.
export const toolDetailSamples = {
  enabled: {
    enabled: true,
    enableAriaLabel: "Enable read_file",
    permissions: [
      {
        key: "read:~/Documents",
        before: "Read files at ",
        code: "~/Documents",
        after: "",
        required: true,
        reason: "Open the files you ask it to summarize.",
        grantState: "granted",
        ariaLabel: "Read files at ~/Documents",
      },
      {
        key: "net:api.example.com:443",
        before: "Network access to ",
        code: "api.example.com:443",
        after: "",
        required: false,
        reason: "Look up reference data while drafting.",
        grantState: "ask",
        ariaLabel: "Network access to api.example.com:443",
      },
    ],
  },
  disabled: {
    enabled: false,
    enableAriaLabel: "Enable run_shell",
    permissions: [
      {
        key: "run:bash",
        before: "Run the ",
        code: "bash",
        after: " command",
        required: true,
        reason: "Execute the commands you approve.",
        grantState: "ask",
        ariaLabel: "Run the bash command",
      },
    ],
  },
  deniedRequired: {
    enabled: true,
    enableAriaLabel: "Enable write_file",
    deniedRequired: 1,
    permissions: [
      {
        key: "write:~/Projects",
        before: "Write files at ",
        code: "~/Projects",
        after: "",
        required: true,
        reason: "Save the edits it makes for you.",
        grantState: "denied",
        ariaLabel: "Write files at ~/Projects",
      },
    ],
  },
  noPermissions: {
    enabled: true,
    enableAriaLabel: "Enable search_web",
    permissions: [],
  },
  enableBusy: {
    enabled: false,
    enableBusy: true,
    enableAriaLabel: "Enable search_web",
    permissions: [],
  },
  alwaysAvailable: {
    enabled: true,
    enableAriaLabel: "Enable get_datetime",
    showAlwaysAvailable: true,
    alwaysAvailable: true,
    alwaysAvailableAriaLabel: "Always offer get_datetime",
    permissions: [],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ToolDetailView>>>;
