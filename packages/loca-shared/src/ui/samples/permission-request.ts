import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type PermissionRequestView from "../components/chat/userinput/PermissionRequestView.svelte";

export const permissionRequestSamples = {
  declared: {
    toolName: "shell",
    action: "run a program",
    detail: "git status",
    declared: true,
  },
  noDetail: {
    toolName: "model",
    action: "use the language model",
    declared: true,
  },
  undeclared: {
    toolName: "fetch",
    action: "connect to a server",
    detail: "https://api.example.com",
    declared: false,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof PermissionRequestView>>>;
