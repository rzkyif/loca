/**
 * State machine + flows for the add-a-core wizard (NewCore.svelte): the steps
 * (connect-local | remote-address -> remote-pair), the per-step form fields,
 * LAN discovery, connection probing, and the local / remote pairing that ends
 * by registering + selecting the new core.
 *
 * The Core itself is installed by the native installer, never from here: when
 * an installer-provided local core exists and none is paired, the wizard opens
 * on `connectLocal` and silently pairs with it (start it if needed, mint a
 * code with the on-disk admin token, claim); otherwise it opens on the
 * welcoming remote-connect form, the same flow mobile always uses.
 *
 * Per the composable convention (see use-autocomplete), this holds the `$state`
 * and imperative flow methods plus pure getters over that state; the component
 * keeps the `$effect`s (the Android back interceptor, the URL stale-check) and
 * the template, and calls `init()` from onMount. `onMobile` is the only piece
 * that comes from the UI context, so it is injected.
 */

import { errMessage } from "@loca/shared";
import {
  cores,
  mintCodeWithAdminToken,
  pairWithCode,
  type PairedCoreEntry,
  probeCore,
} from "$lib/core";
import { type DiscoveredCore, platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { modelRecommendState, viewState } from "$stores";

const log = getLogger("cores");
const CLIENT_NAME = "Loca Client";

// Wizard steps. `connectLocal` is the silent first-boot pair with an
// installer-provided local core (auto-run; its UI is a spinner and, on
// failure, an error with a remote fallback). Everyone else starts at
// `remoteAddress`.
export type WizardView = "connectLocal" | "remoteAddress" | "remotePair";

type ConnectionStatus =
  | { kind: "idle" }
  | { kind: "ok"; version: string; checkedUrl: string; behindProxy: boolean }
  | { kind: "error"; message: string };

export function hostFromUrl(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return "Remote Core";
  }
}

export class NewCoreWizard {
  // Resolved from the platform on mount so a latest client targets the latest
  // core's port (7810) rather than the stable 7800. Falls back to the stable
  // default until resolved (and on the web stub).
  localBaseUrl = $state("https://127.0.0.1:7800");

  busy = $state<null | "claiming" | "checking">(null);
  error = $state("");
  remoteUrl = $state("");
  remoteName = $state("");
  remoteCode = $state("");
  connectionStatus = $state<ConnectionStatus>({ kind: "idle" });

  // LAN "ping" discovery: the address-field search button sweeps the local
  // network for reachable cores and lists them below the field. `discovered`
  // holds the last sweep's results; `didSweep` gates the (animated) results
  // panel so a finished sweep with no hits still shows the empty state.
  discovering = $state(false);
  didSweep = $state(false);
  discovered = $state<DiscoveredCore[]>([]);

  view = $state<WizardView>("remoteAddress");

  constructor(private readonly onMobile: boolean) {}

  // Whether an intra-wizard step-back is available (same condition as the header
  // back arrow). Drives both the arrow and the Android back interceptor.
  get canStepBack(): boolean {
    return this.view === "remotePair";
  }

  // Suggested name shown as the placeholder on the (optional) name field: the
  // host portion of the address we just connected to.
  get defaultRemoteName(): string {
    return hostFromUrl(this.normalizedRemoteUrl());
  }

  // Called from the consumer's onMount: pick the first step (auto-running the
  // local pair when it applies) and resolve the local core base URL.
  init(): void {
    void this.decideInitialView();
    void platform()
      .pairing.localCoreBaseUrl()
      .then((url) => {
        this.localBaseUrl = url;
      })
      .catch((e) => log.warn("localCoreBaseUrl failed", e));
  }

  // Pick the first step. First boot (zero cores paired) on a desktop with an
  // installer-provided local core connects to it silently; everything else
  // (mobile, no local core on disk, any core already paired - including the
  // add-another-core entry from Settings) is the remote-connect form.
  async decideInitialView(): Promise<void> {
    const list = await cores().list();

    let localInstalled = false;
    if (!this.onMobile && list.length === 0) {
      try {
        localInstalled = await platform().pairing.isLocalCoreInstalled();
      } catch {
        localInstalled = false;
      }
    }

    if (localInstalled) {
      this.view = "connectLocal";
      void this.connectLocal();
    } else {
      this.view = "remoteAddress";
    }

    // Prefill the remote fields from launch arguments (--core-url /
    // --pairing-code), as supplied by a shareable setup command or by what
    // `deno task dev` passes. Only fills empty fields, never clobbers typed
    // input.
    try {
      const pre = await platform().pairing.launchPrefill();
      if (pre?.coreUrl) {
        if (!this.remoteUrl.trim()) this.remoteUrl = pre.coreUrl;
        if (!this.remoteCode.trim() && pre.pairingCode) {
          this.remoteCode = pre.pairingCode;
        }
      }
    } catch {
      /* no launch prefill available */
    }
  }

  // A local pair can fail because the core died on boot (e.g. its port is
  // taken), which surfaces here only as a connection timeout. The core leaves a
  // one-line breadcrumb; fold it into the message so the user sees the real
  // cause instead of a bare "could not reach the core".
  private async localPairErrorMessage(e: unknown): Promise<string> {
    const base = errMessage(e);
    try {
      const boot = await platform().pairing.readLocalCoreBootError();
      if (boot) return `${base}\n\nThe local Core failed to start: ${boot}`;
    } catch {
      /* best-effort: the breadcrumb is a nicety, not required */
    }
    return base;
  }

  // Claim a pairing code, register the core, select it, and route onward.
  private async claimAndAdd(
    baseUrl: string,
    code: string,
    name: string,
    isLocal = false,
    // Whether the Core is served over HTTPS behind a terminating proxy (from the
    // health probe). A local/loopback core is always self-signed, so false.
    behindProxy = false,
  ): Promise<void> {
    const firstEver = (await cores().list()).length === 0;
    const res = await pairWithCode(baseUrl, CLIENT_NAME, code, behindProxy);
    const entry: PairedCoreEntry = {
      id: res.clientId,
      name,
      baseUrl,
      trustMode: res.trustMode,
      tlsPin: res.tlsPin,
      addedAtMs: Date.now(),
    };
    await cores().addPaired(entry, res.token);
    // select() notifies the registry, which makes settingsState load the new
    // core's settings baseline (and live-sync from there); nothing to fetch
    // here. Edits made before the baseline lands are queued, not dropped.
    await cores().select(entry.id);
    // First-ever local core: apply the hardware-fit Smallest preset now, before
    // the pending-downloads gate surfaces, so a fresh install lands on the model
    // that best fits this device (and the stored preset is truthful) instead of
    // the static baseline default. Best-effort and awaited so the requirements
    // recompute reflects the chosen model: applyBucket swallows its own errors,
    // so if the catalog/hardware probe isn't ready the realigned static default
    // holds and the user can still pick a preset in Settings.
    if (isLocal && firstEver) {
      // Best-effort and time-bounded: applying the hardware-fit preset needs the
      // signed model catalog, which on a brand-new core may require a network
      // fetch (no cache yet). Never let a slow/unreachable fetch hang onboarding
      // on the spinner: the realigned static default is already an accepted 2B
      // baseline, so fall through after a short wait. applyBucket swallows its
      // own errors and, if it lands later, still applies in the background.
      await Promise.race([
        modelRecommendState.applyBucket("smallest"),
        new Promise<void>((resolve) => setTimeout(resolve, 8000)),
      ]);
    }
    this.busy = null;
    if (firstEver) {
      // First core ever paired: unlock the UI and open the new-user quick
      // settings.
      viewState.setLocked(false);
      viewState.navigate("quickSettings");
    } else {
      // Adding an additional core: return to the Cores manager in Settings.
      viewState.pendingSettingsGroup = "cores";
      viewState.navigate("settings");
    }
  }

  // Step-back within the wizard: remote pairing -> remote address. Only wired
  // to the back arrow, which is hidden when no previous step exists, so it
  // never needs to handle the root case.
  goBack(): void {
    if (this.busy !== null) return;
    this.error = "";
    this.view = "remoteAddress";
  }

  // Explicit exit out of the whole wizard, bound to the close button shown on
  // every step while not locked. Returns to the Cores manager it was launched
  // from. Never reachable while locked (onboarding has nowhere to go).
  exitFlow(): void {
    viewState.pendingSettingsGroup = "cores";
    viewState.navigate("settings");
  }

  // Silently pair with the installer-provided local core: make sure it's
  // running, mint a pairing code via its on-disk admin token, and claim.
  // Auto-run when the wizard opens on `connectLocal`; also re-run by the
  // error state's "Try Again".
  async connectLocal(): Promise<void> {
    this.error = "";
    this.busy = "claiming";
    try {
      // Resolve the channel-aware base URL before probing: init() resolves it
      // concurrently, and this auto-runs immediately on mount, so waiting on
      // that race would sometimes target the wrong channel's port.
      try {
        this.localBaseUrl = await platform().pairing.localCoreBaseUrl();
      } catch {
        /* keep the stable default */
      }
      await platform().pairing.startLocalCore();
      const adminToken = await platform().pairing.readAdminToken();
      if (!adminToken) {
        throw new Error(
          "Local Core admin token not found. The install may be corrupt. " +
            "Reinstall the Core with the Loca installer and try again.",
        );
      }
      const { code } = await mintCodeWithAdminToken(this.localBaseUrl, adminToken);
      if (!code) throw new Error("response missing pairing code");
      await this.claimAndAdd(this.localBaseUrl, code, "Local Core", true);
    } catch (e) {
      this.error = await this.localPairErrorMessage(e);
      this.busy = null;
    }
  }

  // From connectLocal's error state: give up on the local core and fall back
  // to the remote-connect form.
  useRemoteInstead(): void {
    this.error = "";
    this.connectionStatus = { kind: "idle" };
    this.view = "remoteAddress";
  }

  normalizedRemoteUrl(): string {
    let u = this.remoteUrl.trim().replace(/\/+$/, "");
    if (!u) return "";
    // TLS-only: coerce http:// -> https://, and default to https:// when the
    // user typed a bare host:port. The core never serves plaintext.
    if (/^http:\/\//i.test(u)) u = u.replace(/^http:\/\//i, "https://");
    else if (!/^https:\/\//i.test(u)) u = `https://${u}`;
    return u;
  }

  async checkConnection(): Promise<void> {
    this.error = "";
    const url = this.normalizedRemoteUrl();
    if (!url) {
      this.connectionStatus = { kind: "error", message: "Enter the Core's address." };
      return;
    }
    this.busy = "checking";
    try {
      const { version, behindProxy } = await probeCore(url);
      this.connectionStatus = { kind: "ok", version, checkedUrl: url, behindProxy };
      // Connection confirmed: advance to the naming + pairing-code stage.
      this.view = "remotePair";
    } catch (e) {
      const message = errMessage(e);
      this.connectionStatus = {
        kind: "error",
        message: `Could not reach Core: ${message}`,
      };
    } finally {
      this.busy = null;
    }
  }

  // Sweep the local network for reachable cores (the address-field "ping"
  // button). Discovery only reads each core's public /api/v1/health; the
  // address it fills still has to clear the full PAKE pairing below, so nothing
  // found here is trusted yet.
  async pingNetwork(): Promise<void> {
    if (this.discovering) return;
    this.error = "";
    this.discovering = true;
    try {
      this.discovered = await platform().net.discoverCores();
    } catch (e) {
      log.warn("core discovery failed", e);
      this.discovered = [];
    } finally {
      this.didSweep = true;
      this.discovering = false;
    }
  }

  // Fill the address field from a discovered core and collapse the list. The
  // user still enters the pairing code on the next step.
  useDiscovered(core: DiscoveredCore): void {
    this.remoteUrl = core.baseUrl;
    this.discovered = [];
    this.didSweep = false;
  }

  async pairRemote(): Promise<void> {
    this.error = "";
    const url = this.normalizedRemoteUrl();
    if (!url) {
      this.error = "Enter the Core's address";
      return;
    }
    if (!/^\d{6}$/.test(this.remoteCode)) {
      this.error = "Pairing code must be 6 digits";
      return;
    }
    if (this.connectionStatus.kind !== "ok" || this.connectionStatus.checkedUrl !== url) {
      this.error = "Check the connection first";
      return;
    }
    const { behindProxy } = this.connectionStatus;
    this.busy = "claiming";
    try {
      await this.claimAndAdd(
        url,
        this.remoteCode,
        this.remoteName.trim() || hostFromUrl(url),
        false,
        behindProxy,
      );
      this.remoteUrl = "";
      this.remoteName = "";
      this.remoteCode = "";
      this.connectionStatus = { kind: "idle" };
    } catch (e) {
      this.error = errMessage(e);
      this.busy = null;
    }
  }
}
