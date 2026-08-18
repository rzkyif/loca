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
  isLocalCoreEntry,
  LOCAL_CORE_ID,
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

// Wizard steps.
//
// `choose` is the fork offered where the app embeds a Core of its own: run it
// here, or connect to one elsewhere. `connectLocal` is the silent first-boot
// pair with an installer-provided local core process (auto-run; its UI is a
// spinner and, on failure, an error with a remote fallback). Everyone else
// starts at `remoteAddress`.
export type WizardView = "choose" | "connectLocal" | "remoteAddress" | "remotePair";

/** The name the reserved entry is recorded under. Not "Local Core": on a device
 *  that hosts its own Core, what distinguishes it from the others in the list is
 *  that it IS this device. */
const LOCAL_CORE_NAME = "This Device";

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

  // Whether the fork is offered at all, and whether "this device" is still on
  // it. Resolved in decideInitialView so the template reads live state rather
  // than re-asking the platform on every render.
  canRunHere = $state(false);

  constructor(private readonly onMobile: boolean) {}

  // Whether an intra-wizard step-back is available (same condition as the header
  // back arrow). Drives both the arrow and the Android back interceptor.
  get canStepBack(): boolean {
    if (this.view === "remotePair") return true;
    // The address form is a step back to the fork only when the fork was shown.
    return this.view === "remoteAddress" && this.canRunHere;
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

  // Pick the first step.
  //
  // Where the app embeds a Core, the user is offered the fork: run it here, or
  // connect to one elsewhere. That gate is a fact about this BUILD (does this
  // device host a Core at all), never a core capability: the core being asked
  // has not been chosen yet, so there is nothing to negotiate with. It is also
  // not a platform sniff, so a desktop build that ever embedded a Core would get
  // the same offer without a second flag.
  //
  // Otherwise: first boot (zero cores) on a desktop with an installer-provided
  // local core connects to it silently, and everything else (no local core on
  // disk, any core already paired, including the add-another-core entry from
  // Settings) is the remote-connect form.
  async decideInitialView(): Promise<void> {
    const list = await cores().list();
    // Already adopted: there is exactly one Core inside this app, so offering to
    // add it again would be an action with nothing to do.
    this.canRunHere = Boolean(platform().localCore) && !list.some(isLocalCoreEntry);

    let localInstalled = false;
    if (!this.onMobile && !this.canRunHere && list.length === 0) {
      try {
        localInstalled = await platform().pairing.isLocalCoreInstalled();
      } catch {
        localInstalled = false;
      }
    }

    if (this.canRunHere) {
      this.view = "choose";
    } else if (localInstalled) {
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
    this.finish(firstEver);
  }

  // Where the wizard goes once a core is added, by whichever route.
  private finish(firstEver: boolean): void {
    this.busy = null;
    if (firstEver) {
      // First core ever added: unlock the UI and open the new-user quick
      // settings.
      viewState.setLocked(false);
      viewState.navigate("quickSettings");
    } else {
      // Adding an additional core: return to the Cores manager in Settings.
      viewState.pendingSettingsGroup = "cores";
      viewState.navigate("settings");
    }
  }

  // Step-back within the wizard: remote pairing -> remote address, and the
  // address form -> the fork when the fork was shown. Only wired to the back
  // arrow, which is hidden when no previous step exists, so it never needs to
  // handle the root case.
  goBack(): void {
    if (this.busy !== null) return;
    this.error = "";
    this.connectionStatus = { kind: "idle" };
    this.view = this.view === "remotePair" ? "remoteAddress" : "choose";
  }

  // Explicit exit out of the whole wizard, bound to the close button shown on
  // every step while not locked. Returns to the Cores manager it was launched
  // from. Never reachable while locked (onboarding has nowhere to go).
  exitFlow(): void {
    viewState.pendingSettingsGroup = "cores";
    viewState.navigate("settings");
  }

  // The "run it here" branch of the fork: adopt the Core inside this app.
  //
  // There is NO pairing here and nothing lands in the keychain. The Core is
  // already this device and the client it serves is pre-authenticated by
  // construction, so adopting it is a registry write followed by a select. That
  // is the whole reason this is not `claimAndAdd`: there is no code to mint, no
  // PAKE to run, and no token to keep.
  //
  // The Core is BOOTED FIRST, and that ordering is the point. `select()` rebinds
  // every persistent WS listener onto the new client, which connects at once, so
  // without this the first thing a fresh install does is measure the Core's
  // first-ever boot (root dirs, SQLite migrations, the Ed25519-verified offline
  // install of the built-in extension) against a connect deadline. Booting
  // first also keeps a Core that cannot start out of the registry: the user gets
  // the reason on the fork instead of a chat view behind a dead connection.
  async useThisDevice(): Promise<void> {
    this.error = "";
    this.busy = "claiming";
    try {
      const firstEver = (await cores().list()).length === 0;
      await platform().pairing.startLocalCore();
      await cores().adoptLocalCore(LOCAL_CORE_NAME);
      await cores().select(LOCAL_CORE_ID);
      // Same hardware-fit step the first local pair does, and it matters more
      // here than anywhere: a phone's baseline is the tightest fit in the
      // catalog. Best-effort and time-bounded, exactly as in claimAndAdd.
      if (firstEver) {
        await Promise.race([
          modelRecommendState.applyBucket("smallest"),
          new Promise<void>((resolve) => setTimeout(resolve, 8000)),
        ]);
      }
      this.finish(firstEver);
    } catch (e) {
      this.error = errMessage(e);
      this.busy = null;
    }
  }

  // The "connect to one elsewhere" branch of the fork.
  useAnotherDevice(): void {
    this.error = "";
    this.view = "remoteAddress";
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
