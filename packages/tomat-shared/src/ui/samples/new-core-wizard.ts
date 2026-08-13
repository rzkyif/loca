import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type NewCoreWizardView from "../components/new-core/NewCoreWizardView.svelte";

const DOCS_URL = "https://au.tomat.ing/manual/maintenance/cores";

export const newCoreWizardSamples = {
  // First boot with an installer-provided local core: the welcome header over
  // the silent auto-pair spinner.
  connectLocal: {
    step: "connectLocal",
    locked: true,
    busy: "claiming",
    coreSetupDocsUrl: DOCS_URL,
  },
  // The auto-pair failed: the error explains itself and offers a retry or the
  // remote fallback.
  connectLocalError: {
    step: "connectLocal",
    locked: true,
    error:
      "Could not reach Core: connection timed out\n\nThe local Core failed to start: port 7800 is already in use",
    coreSetupDocsUrl: DOCS_URL,
  },
  // First boot without a local core: the welcoming remote-connect form, with a
  // finished sweep listing one discovered core.
  remoteAddress: {
    step: "remoteAddress",
    locked: true,
    coreSetupDocsUrl: DOCS_URL,
    didSweep: true,
    discovered: [{ pin: "a1b2", hostLabel: "studio.local:7800", version: "0.4.1" }],
    remoteUrl: "https://192.168.1.20:7800",
  },
  // Remote step 2: name + 6-digit pairing code (valid, Pair button shown).
  remotePair: {
    step: "remotePair",
    canStepBack: true,
    coreSetupDocsUrl: DOCS_URL,
    defaultRemoteName: "192.168.1.20:7800",
    remoteName: "Studio Core",
    remoteCode: "482915",
    remoteCodeValid: true,
  },
  // Connecting: checking a remote address (spinner on the Check button).
  connecting: {
    step: "remoteAddress",
    coreSetupDocsUrl: DOCS_URL,
    busy: "checking",
    remoteUrl: "https://192.168.1.20:7800",
  },
  // Error: a failed connection check on the remote-address step.
  error: {
    step: "remoteAddress",
    coreSetupDocsUrl: DOCS_URL,
    remoteUrl: "https://192.168.1.20:7800",
    connectionError: "Could not reach Core: connection timed out",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof NewCoreWizardView>>>;
