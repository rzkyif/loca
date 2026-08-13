// Paired-cores registry. Backed by its own cores.json (the {id, name, baseUrl,
// trustMode, tlsPin, addedAtMs} entries plus the currentCoreId pointer) + OS keychain
// (the bearer tokens). Owns the currently-selected core and rebuilds the
// CoreClient + its per-domain APIs on switch. This module is the file's only
// writer; settings live in their own settings.json (see lib/platform).

import { platform } from "../platform/index.ts";
import { getLogger } from "$lib/util/log";
import { Subscribers } from "../util/subscribers.ts";
import { BinariesApi } from "./binaries";
import { ChatApi } from "./chat";
import { type ConnectionListener, CoreClient, type TrustMode, type WsListener } from "./client";
import { GreetingsApi } from "./greetings";
import { LlmApi } from "./llm";
import { MemoriesApi } from "./memories";
import { ScheduledPromptsApi } from "./scheduled-prompts";
import { ModelsApi } from "./models";
import { RequirementsApi } from "./requirements";
import { SidecarsApi } from "./sidecars";
import { StorageApi } from "./storage";
import { PairingApi } from "./pairing";
import { CoreSettingsApi } from "./settings";
import { SessionsApi } from "./sessions";
import { SttApi } from "./stt";
import { ExtensionsApi } from "./extensions";
import { McpApi } from "./mcp";
import { TtsApi } from "./tts";
import { UpdateApi } from "./update";

const log = getLogger("cores");

export interface PairedCoreEntry {
  id: string; // ULID returned by pairing
  name: string; // user-visible label
  baseUrl: string; // e.g. https://127.0.0.1:7800
  trustMode: TrustMode; // "pin" (self-signed, default) | "webpki" (behind an HTTPS proxy)
  tlsPin?: string; // pinned cert SPKI (base64 SHA-256), captured at pairing; present iff trustMode === "pin"
  addedAtMs: number;
}

/** Validate one raw cores.json entry. A `webpki` core carries no pin; every
 *  other entry is `pin` mode and MUST carry a non-empty pin - a pin-mode entry
 *  without a pin would drive an accept-any connection, so it is dropped (the
 *  user re-pairs) rather than trusted. Returns null for anything malformed. */
function coerceCoreEntry(raw: unknown): PairedCoreEntry | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.name !== "string" ||
    typeof r.baseUrl !== "string" ||
    typeof r.addedAtMs !== "number"
  ) {
    return null;
  }
  if (r.trustMode === "webpki") {
    return {
      id: r.id,
      name: r.name,
      baseUrl: r.baseUrl,
      trustMode: "webpki",
      addedAtMs: r.addedAtMs,
    };
  }
  if (typeof r.tlsPin !== "string" || r.tlsPin.length === 0) return null;
  return {
    id: r.id,
    name: r.name,
    baseUrl: r.baseUrl,
    trustMode: "pin",
    tlsPin: r.tlsPin,
    addedAtMs: r.addedAtMs,
  };
}

const CURRENT_KEY = "currentCoreId";

class CoresRegistry {
  private current: { entry: PairedCoreEntry; client: CoreClient } | null = null;
  private listeners = new Subscribers<() => void>();
  // Persistent WS / connection-state listeners. These survive core switches:
  // subscribeWs / subscribeConnectionState add to these sets, and select()
  // re-binds every one onto the freshly-built client. Without this, a listener
  // registered while no core was paired (or before a switch) would be bound to
  // a dead/closed client and silently stop receiving frames. The *Bindings
  // maps hold each listener's unsubscribe handle for the CURRENT client only.
  private wsListeners = new Set<WsListener>();
  private wsBindings = new Map<WsListener, () => void>();
  private connListeners = new Set<ConnectionListener>();
  private connBindings = new Map<ConnectionListener, () => void>();
  private apis: {
    sessions: SessionsApi;
    chat: ChatApi;
    models: ModelsApi;
    binaries: BinariesApi;
    requirements: RequirementsApi;
    sidecars: SidecarsApi;
    storage: StorageApi;
    extensions: ExtensionsApi;
    memories: MemoriesApi;
    mcp: McpApi;
    scheduledPrompts: ScheduledPromptsApi;
    greetings: GreetingsApi;
    stt: SttApi;
    tts: TtsApi;
    llm: LlmApi;
    settings: CoreSettingsApi;
    pairing: PairingApi;
    update: UpdateApi;
  } | null = null;

  // --- list / select -----------------------------------------------------

  async list(): Promise<PairedCoreEntry[]> {
    return (await this.readRegistry()).cores;
  }

  async addPaired(entry: PairedCoreEntry, token: string): Promise<void> {
    await platform().keychain.set(entry.id, token);
    // Verify the token actually landed before recording the core. A keychain
    // write can fail silently (access denied, the keychain is locked) on some
    // platforms; without this read-back the entry would persist with an
    // unreadable token and surface only later as a dead, unrecoverable
    // connection. Failing here keeps the user in the pairing flow where a retry
    // is obvious.
    const stored = await platform().keychain.get(entry.id);
    if (stored !== token) {
      throw new Error(
        "Could not save this Core's access token to the system keychain. " +
          "Grant keychain access (or unlock it) and pair again.",
      );
    }
    const cores = await this.list();
    const updated = cores.filter((c) => c.id !== entry.id).concat(entry);
    await this.writeCores(updated, entry.id);
  }

  async removePaired(id: string): Promise<void> {
    await platform().keychain.delete(id);
    const cores = await this.list();
    const remaining = cores.filter((c) => c.id !== id);
    const nextCurrent = remaining[0]?.id;
    await this.writeCores(remaining, nextCurrent);
    if (this.current?.entry.id === id) {
      this.current.client.close();
      this.current = null;
      this.apis = null;
      // The closed client's subscriptions are dead; the persistent listener
      // sets stay intact so a later select() re-binds them.
      this.wsBindings.clear();
      this.connBindings.clear();
    }
    this.notify();
  }

  /** Rename a paired core in place. Preserves the current-core pointer and
   *  keeps the in-memory active entry's name in sync. */
  async rename(id: string, name: string): Promise<void> {
    const { cores, currentCoreId } = await this.readRegistry();
    if (!cores.some((c) => c.id === id)) return;
    await this.writeCores(
      cores.map((c) => (c.id === id ? { ...c, name } : c)),
      currentCoreId,
    );
    if (this.current?.entry.id === id) {
      this.current = {
        ...this.current,
        entry: { ...this.current.entry, name },
      };
    }
    this.notify();
  }

  async select(id: string): Promise<void> {
    const cores = await this.list();
    const entry = cores.find((c) => c.id === id);
    if (!entry) throw new Error(`core ${id} not in paired list`);
    const token = await platform().keychain.get(entry.id);
    if (!token) throw new Error(`no token for core ${entry.id}; re-pair`);
    if (this.current) this.current.client.close();
    const client = new CoreClient(
      {
        baseUrl: entry.baseUrl,
        token,
        trustMode: entry.trustMode,
        tlsPin: entry.tlsPin,
      },
      () => isThisDevicesCoreStopped(entry.baseUrl),
    );
    this.current = { entry, client };
    this.apis = {
      sessions: new SessionsApi(client),
      chat: new ChatApi(client),
      models: new ModelsApi(client),
      binaries: new BinariesApi(client),
      requirements: new RequirementsApi(client),
      sidecars: new SidecarsApi(client),
      storage: new StorageApi(client),
      extensions: new ExtensionsApi(client),
      memories: new MemoriesApi(client),
      mcp: new McpApi(client),
      scheduledPrompts: new ScheduledPromptsApi(client),
      greetings: new GreetingsApi(client),
      stt: new SttApi(client),
      tts: new TtsApi(client),
      llm: new LlmApi(client),
      settings: new CoreSettingsApi(client),
      pairing: new PairingApi(client),
      update: new UpdateApi(client),
    };
    this.rebindListeners(client);
    await this.writeCurrent(id);
    this.notify();
  }

  /** Re-subscribe every persistent WS / connection-state listener onto a
   *  freshly-built client. The previous client was closed by `select()`, so
   *  its subscriptions are already dead, so drop the stale handles and rebind. */
  private rebindListeners(client: CoreClient): void {
    this.wsBindings.clear();
    this.connBindings.clear();
    for (const l of this.wsListeners) {
      this.wsBindings.set(l, client.subscribe(l));
    }
    for (const l of this.connListeners) {
      this.connBindings.set(l, client.onConnectionState(l));
    }
  }

  async restoreSelected(): Promise<void> {
    const { cores, currentCoreId: currentId } = await this.readRegistry();
    if (cores.length === 0) return;
    // Try the persisted current first, then the rest, and stop at the first that
    // selects. A single broken pairing (missing token after a reset) must not
    // strand a usable sibling. If none select, the caller (boot) recovers: see
    // the tokenless-core cleanup in +page.svelte.
    const ordered = [
      ...cores.filter((c) => c.id === currentId),
      ...cores.filter((c) => c.id !== currentId),
    ];
    for (const entry of ordered) {
      try {
        await this.select(entry.id);
        return;
      } catch (e) {
        // Don't abort boot. The window is already up. Surface the failure
        // (keychain miss, missing token, ...) instead of swallowing it.
        log.error(`restoreSelected: core ${entry.id} not usable:`, e);
      }
    }
  }

  /** Close the active client and drop all per-client state. Used by the HMR
   *  dispose hook below so a hot-replaced module instance doesn't leave its
   *  WS client reconnecting in the background. */
  dispose(): void {
    this.current?.client.close();
    this.current = null;
    this.apis = null;
    this.wsBindings.clear();
    this.connBindings.clear();
  }

  currentEntry(): PairedCoreEntry | null {
    return this.current?.entry ?? null;
  }

  currentClient(): CoreClient | null {
    return this.current?.client ?? null;
  }

  api(): {
    sessions: SessionsApi;
    chat: ChatApi;
    models: ModelsApi;
    binaries: BinariesApi;
    requirements: RequirementsApi;
    sidecars: SidecarsApi;
    storage: StorageApi;
    extensions: ExtensionsApi;
    memories: MemoriesApi;
    mcp: McpApi;
    scheduledPrompts: ScheduledPromptsApi;
    greetings: GreetingsApi;
    stt: SttApi;
    tts: TtsApi;
    llm: LlmApi;
    settings: CoreSettingsApi;
    pairing: PairingApi;
    update: UpdateApi;
  } {
    if (!this.apis) throw new Error("no core selected; call select() first");
    return this.apis;
  }

  subscribe(listener: () => void): () => void {
    return this.listeners.add(listener);
  }

  subscribeWs(listener: WsListener): () => void {
    this.wsListeners.add(listener);
    const c = this.current?.client;
    if (c) this.wsBindings.set(listener, c.subscribe(listener));
    return () => {
      this.wsListeners.delete(listener);
      this.wsBindings.get(listener)?.();
      this.wsBindings.delete(listener);
    };
  }

  subscribeConnectionState(listener: ConnectionListener): () => void {
    this.connListeners.add(listener);
    const c = this.current?.client;
    if (c) this.connBindings.set(listener, c.onConnectionState(listener));
    return () => {
      this.connListeners.delete(listener);
      this.connBindings.get(listener)?.();
      this.connBindings.delete(listener);
    };
  }

  // --- internals ---------------------------------------------------------

  private async readRegistry(): Promise<{
    cores: PairedCoreEntry[];
    currentCoreId?: string;
  }> {
    const raw = await platform().clientFiles.read("cores");
    const rawCores = Array.isArray(raw.cores) ? raw.cores : [];
    const cores: PairedCoreEntry[] = [];
    for (const c of rawCores) {
      const entry = coerceCoreEntry(c);
      if (entry) cores.push(entry);
      else log.warn("dropping a malformed core entry from cores.json (re-pair to restore it)");
    }
    const currentCoreId =
      typeof raw[CURRENT_KEY] === "string" ? (raw[CURRENT_KEY] as string) : undefined;
    return { cores, currentCoreId };
  }

  private async writeCores(cores: PairedCoreEntry[], current?: string): Promise<void> {
    // This registry is cores.json's single owner, so a full write is safe.
    // An absent currentCoreId (last core removed, or transition through
    // no-current) is simply omitted so the next restoreSelected() doesn't
    // search for a ghost id.
    await platform().clientFiles.write("cores", {
      cores,
      ...(current ? { [CURRENT_KEY]: current } : {}),
    });
  }

  private async writeCurrent(id: string): Promise<void> {
    const { cores } = await this.readRegistry();
    await this.writeCores(cores, id);
  }

  private notify(): void {
    this.listeners.emit();
  }
}

let _instance: CoresRegistry | null = null;
export function cores(): CoresRegistry {
  if (!_instance) _instance = new CoresRegistry();
  return _instance;
}

/** A paired core is "local" when it pairs over loopback (the client-managed
 *  core on this device); remote cores use a LAN/WAN host. Matches the parsed
 *  host EXACTLY (not a substring): a remote core whose name merely contains
 *  "localhost"/"127.0.0.1" - e.g. "localhost.example.com" or "127.0.0.1.evil" -
 *  must NOT read as local, since pruneUninstalledLocalCores would then delete it.
 *  A URL that does not parse is treated as remote (never pruned). */
export function isLoopbackUrl(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname;
  } catch {
    return false;
  }
  // URL.hostname brackets IPv6 literals ("[::1]"); unwrap before comparing.
  const bare = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
  return bare === "localhost" || bare === "::1" || /^127(?:\.\d{1,3}){3}$/.test(bare);
}

/** Whether a paired core IS the core installed on this device, i.e. its base
 *  URL matches the platform-resolved local core origin exactly. This is the
 *  gate for handing out the on-disk admin token (promptless code minting,
 *  password management): a plain loopback check is NOT enough, because a
 *  REMOTE core tunneled through 127.0.0.1 must never receive this device's
 *  admin credential. */
export function isThisDevicesCore(coreBaseUrl: string, localBaseUrl: string | null): boolean {
  if (!localBaseUrl) return false;
  try {
    return new URL(coreBaseUrl).origin === new URL(localBaseUrl).origin;
  } catch {
    return false;
  }
}

/** Whether `coreBaseUrl` is this device's own core AND that core's process is
 *  not running: the one case where a reconnect cannot succeed but the user can
 *  fix it by starting the core. Gates on `isThisDevicesCore`, not
 *  `isLoopbackUrl`: a REMOTE core tunneled through 127.0.0.1 is loopback but is
 *  not ours to start, so it must follow the ordinary reconnect path. Injected
 *  into every CoreClient (see `select`) as its local-stopped probe. */
export async function isThisDevicesCoreStopped(coreBaseUrl: string): Promise<boolean> {
  const local = await platform().pairing.localCoreBaseUrl();
  if (!isThisDevicesCore(coreBaseUrl, local)) return false;
  return !(await platform().pairing.isLocalCoreRunning());
}

// On-demand mode: when the selected core points at loopback and the binary
// is installed locally, spawn it ourselves if no service has it running.
// Idempotent: start_local_core probes the port first and exits cleanly if
// the core is already up. Failures are non-fatal: the user sees a normal
// "could not reach core" error from the regular call paths.
export async function ensureLocalCoreUpIfNeeded(): Promise<void> {
  const current = cores().currentEntry();
  if (!current) return;
  if (!isLoopbackUrl(current.baseUrl)) return;
  try {
    if (await platform().pairing.isLocalCoreInstalled()) {
      await platform().pairing.startLocalCore();
    }
  } catch (e) {
    log.error("ensureLocalCoreUpIfNeeded:", e);
  }
}

/** Drop paired LOCAL cores whose core is no longer installed on this device.
 *  When the local core was uninstalled - the user removed both parts and
 *  reinstalled just the Client, or removed the Core on its own - its paired
 *  record points at a core that is gone, and the app would otherwise keep
 *  trying to reach it. Keys off the on-disk core binary (isLocalCoreInstalled),
 *  NOT reachability, so a merely stopped local core is never dropped and an
 *  update (which leaves the binary in place) never triggers it. Best-effort: if
 *  the install check throws, the records are left paired rather than risk
 *  unpairing a still-valid core. Run before restoreSelected so a gone local core
 *  is never selected or connected to. */
export async function pruneUninstalledLocalCores(): Promise<void> {
  const locals = (await cores().list()).filter((c) => isLoopbackUrl(c.baseUrl));
  if (locals.length === 0) return;
  let installed: boolean;
  try {
    installed = await platform().pairing.isLocalCoreInstalled();
  } catch (e) {
    log.error("local-core install check failed; leaving local cores paired:", e);
    return;
  }
  if (installed) return;
  for (const c of locals) {
    log.warn(`local core "${c.name}" is no longer installed on disk; unpairing`);
    try {
      await cores().removePaired(c.id);
    } catch (e) {
      log.error("auto-unpair of uninstalled local core failed:", e);
    }
  }
}

if (import.meta.hot) {
  // HMR re-evaluates this module (e.g. an upstream @tomat/shared edit) and resets
  // _instance, orphaning the prior registry's still-reconnecting WS client. Close
  // it before the old instance is discarded so dev sockets don't pile up and
  // reconnect in lockstep on every save.
  import.meta.hot.dispose(() => _instance?.dispose());
}
