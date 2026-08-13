import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type CoreBarView from "../components/chat/CoreBarView.svelte";

const CORES = [
  { id: "laptop", name: "Laptop" },
  { id: "desktop", name: "Desktop" },
  { id: "studio", name: "Mac Studio" },
];

export const coreBarSamples = {
  idle: {
    status: "idle",
    cores: CORES.slice(0, 1),
    currentCoreId: "laptop",
  },
  busy: {
    status: "busy",
    detail: "1 queued",
    queues: {
      llmActive: 1,
      llmQueued: 1,
      speechActive: 0,
      speechQueued: 0,
      activeStreams: 1,
    },
    cores: CORES,
    currentCoreId: "laptop",
  },
  busyExpanded: {
    status: "busy",
    detail: "1 queued",
    expanded: true,
    queues: {
      llmActive: 1,
      llmQueued: 1,
      speechActive: 1,
      speechQueued: 0,
      activeStreams: 2,
    },
    cores: CORES,
    currentCoreId: "laptop",
  },
  downloading: {
    status: "downloading",
    detail: "2 files",
    progress: 0.35,
    cores: CORES.slice(0, 1),
    currentCoreId: "laptop",
  },
  startingUp: {
    status: "starting_up",
    detail: "loading speech",
    progress: 0.4,
    subsystems: [{ kind: "speech", status: "Loading" }],
    cores: CORES.slice(0, 1),
    currentCoreId: "laptop",
  },
  error: {
    status: "error",
    detail: "exited with code 1",
    subsystems: [{ kind: "llama", status: "Error", message: "exited with code 1" }],
    cores: CORES,
    currentCoreId: "desktop",
  },
  errorExpanded: {
    status: "error",
    detail: "exited with code 1",
    expanded: true,
    subsystems: [
      {
        kind: "llama",
        status: "Error",
        message: "llama-server: failed to load model: out of memory",
      },
      { kind: "speech", status: "Error", message: "whisper model not found on disk" },
    ],
    cores: CORES,
    currentCoreId: "desktop",
  },
  reconnecting: {
    status: "reconnecting",
    cores: CORES,
    currentCoreId: "studio",
  },
  // The two settled failures. Both render an actionable pill (a button that
  // retries, or starts this device's core) rather than an expandable one.
  disconnected: {
    status: "disconnected",
    detail: "Connection refused",
    cores: CORES,
    currentCoreId: "studio",
  },
  stopped: {
    status: "stopped",
    cores: CORES.slice(0, 1),
    currentCoreId: "laptop",
  },
  multiCore: {
    status: "idle",
    cores: CORES,
    currentCoreId: "desktop",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof CoreBarView>>>;
