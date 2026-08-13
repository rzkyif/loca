import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ServicesFieldView from "../components/settings/ServicesFieldView.svelte";

export const servicesFieldSamples = {
  // Core scope: the core process plus two running sidecars, all reporting.
  running: {
    rows: [
      { label: "Core Service", sub: undefined, cpuText: "0.4%", ramText: "82 MB" },
      {
        label: "Language Model",
        sub: "Running · 127.0.0.1:7701",
        cpuText: "12.7%",
        ramText: "1.2 GB",
      },
      {
        label: "Speech",
        sub: "Running · 127.0.0.1:7702",
        cpuText: "1.1%",
        ramText: "210 MB",
      },
    ],
    totalCpuText: "14.2%",
    totalRamText: "1.5 GB",
  },
  // An errored sidecar offers a Retry control next to its status line.
  errored: {
    rows: [
      { label: "Core Service", sub: undefined, cpuText: "0.3%", ramText: "80 MB" },
      {
        label: "Language Model",
        sub: "Error · 127.0.0.1:7701",
        cpuText: "-",
        ramText: "-",
        retryKind: "llama",
        retryLabel: "Retry",
        retryDisabled: false,
      },
    ],
    totalCpuText: "0.3%",
    totalRamText: "80 MB",
  },
  // Client scope: only the desktop app process; no sidecars.
  clientOnly: {
    rows: [{ label: "Main Application", sub: undefined, cpuText: "2.0%", ramText: "320 MB" }],
    totalCpuText: "2.0%",
    totalRamText: "320 MB",
  },
  // No paired core / nothing running yet: header and totals fall back to dashes.
  empty: {
    rows: [],
    totalCpuText: "-",
    totalRamText: "-",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ServicesFieldView>>>;
