import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type CoresFieldView from "../components/settings/CoresFieldView.svelte";

export const coresFieldSamples = {
  current: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    devices: [
      { id: "d1", name: "Studio Mac", lastSeenLabel: "active now", isMe: true },
      { id: "d2", name: "Work Laptop", lastSeenLabel: "last seen 3 minutes ago", isMe: false },
      { id: "d3", name: "Pixel 8", lastSeenLabel: "last seen 2 hours ago", isMe: false },
    ],
  },
  // A same-device Core fresh from the installer: no admin password yet, so
  // codes mint without a prompt and the password section offers to set one.
  localNoPassword: {
    draftName: "Local Core",
    baseUrl: "https://127.0.0.1:7800",
    isCurrent: true,
    adminPasswordSet: false,
    canSetPassword: true,
    devices: [{ id: "d1", name: "This Mac", lastSeenLabel: "active now", isMe: true }],
  },
  // The same Core once a password exists: the section flips to replace mode.
  localWithPassword: {
    draftName: "Local Core",
    baseUrl: "https://127.0.0.1:7800",
    isCurrent: true,
    adminPasswordSet: true,
    canSetPassword: true,
    devices: [{ id: "d1", name: "This Mac", lastSeenLabel: "active now", isMe: true }],
  },
  mintedCode: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    mintedCode: "428 913",
    mintedExpiresLabel: "expires in 5 minutes",
    devices: [{ id: "d1", name: "Studio Mac", lastSeenLabel: "active now", isMe: true }],
  },
  noDevices: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    devices: [],
  },
  devicesLoading: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    devices: null,
  },
  devicesError: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    devices: null,
    devicesError: "Could not reach the Core.",
  },
  codeCopied: {
    draftName: "Studio Mac",
    baseUrl: "https://192.168.1.20:8787",
    isCurrent: true,
    mintedCode: "428 913",
    mintedExpiresLabel: "expires in 5 minutes",
    codeCopied: true,
    devices: [{ id: "d1", name: "Studio Mac", lastSeenLabel: "active now", isMe: true }],
  },
  remote: {
    draftName: "Home Server",
    baseUrl: "https://core.example.com:8787",
    isCurrent: false,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof CoresFieldView>>>;
