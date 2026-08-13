/**
 * Reactive store for user settings, layered for the core/client split:
 *
 *   merged view  = schema defaults + clientSparse + coreSparse
 *   clientSparse = non-default client-destination keys, persisted to
 *                  ~/.loca/<channel>/client/settings.json (this store is the
 *                  file's only owner; cores.json and snippets/ are separate).
 *   coreSparse   = non-default core-destination keys, persisted to the
 *                  currently-selected core via PATCH /settings and kept live
 *                  by the core's settings.updated WS broadcasts.
 *
 * Every mutation, whatever its origin (a user edit, a file load, a core
 * baseline GET, a WS delta from another client, a core-switch reset), flows
 * through one pipeline (`applyChanges`) that updates the layers, mutates the
 * merged record per key, and notifies `onChange` listeners for keys whose
 * effective value actually changed. Value-diffing makes WS echoes of our own
 * PATCHes, repeated loads, and listener writebacks converge to no-ops.
 *
 * Listeners therefore fire at APPLY time (optimistically for user edits), and
 * a failed persist fires the reverse transition when the batch rolls back.
 * Persist failures are isolated per destination: a core PATCH failure rolls
 * back only core keys, a file-write failure only client keys.
 *
 * Core-destination edits made before the core baseline has loaded (e.g. in
 * quick settings right after pairing) are queued, not dropped: the baseline
 * skips locally-edited keys and the queued delta is PATCHed right after it
 * lands. Secret-typed fields never enter the layers or the wire: only names
 * the user actually edited are written, straight to the secrets vault.
 */

import { browser, dev } from "$app/environment";
import {
  type SettingDestination,
  destinationNeedsCore,
  getDefaultSettings,
  initialThinkingBudgetAlignment,
  isClientGroup,
  isCoreGroup,
  SECRET_KEYS,
  type SettingGroupId,
  settingKeyDestination,
} from "@loca/shared";
import { platform } from "$lib/platform";
import { cores } from "$lib/core";
import { getLogger } from "$lib/util/log";
import type { Alignment } from "$lib/util/types";

const log = getLogger("settings");

export type SettingsChangeOrigin = "user" | "load" | "remote";

type SettingChangeListener = (
  key: string,
  prev: unknown,
  next: unknown,
  origin: SettingsChangeOrigin,
) => void | Promise<void>;

const SECRET_KEY_SET = new Set<string>(SECRET_KEYS);

// Schema defaults, captured once: every value is a scalar, so sharing one
// copy for lookups is safe. The merged $state record gets its own copy in the
// constructor. A platform default overlay (setPlatformDefaults) may raise some
// keys at boot, so this is not frozen.
const DEFAULTS: Record<string, unknown> = getDefaultSettings();

function destinationFor(key: string): SettingDestination {
  return settingKeyDestination(key) ?? "client-on-client";
}

// "Core key" here means "persisted on the core" (the shared `core` store OR the
// per-client `client-on-core` overlay) - i.e. it rides the coreSparse layer and
// the core PATCH/baseline/WS machinery, as opposed to the local client file.
function isCoreKey(key: string): boolean {
  return key in DEFAULTS && !SECRET_KEY_SET.has(key) && destinationNeedsCore(destinationFor(key));
}

// Debounce window for coalescing rapid edits into a single round-trip.
// 200ms is short enough that the user perceives saves as immediate but
// long enough that a flurry of keystrokes in one text field collapses
// into one PATCH instead of one PATCH per character.
const FLUSH_DEBOUNCE_MS = 200;

interface Transition {
  key: string;
  prev: unknown;
  next: unknown;
}

class SettingsState {
  // The merged view every reader consumes. Mutated PER KEY (never replaced
  // wholesale) so Svelte's fine-grained reactivity invalidates only the keys
  // that changed.
  // deno-lint-ignore no-explicit-any -- consumers treat values as untyped
  // and the schema-defaults loader builds a heterogeneous record.
  currentSettings = $state<Record<string, any>>(getDefaultSettings());

  // Names of secret-typed settings (API keys) the core reports as configured.
  // Core never returns secret VALUES, so the field stays empty in the UI; this
  // set lets password fields render a "saved" placeholder. Loaded from
  // GET /settings/secrets and kept live by settings.updated secretNames.
  configuredSecrets = $state<Set<string>>(new Set());

  // True once the selected core's baseline has been merged. Until then core
  // PATCHes are held (edits queue in pendingCoreEdits) because a diff against
  // an unknown baseline could overwrite the core's real values.
  coreLoaded = $state(false);

  // Sparse layers. Plain objects (not $state): the merged record is the
  // reactive surface; these are persistence bookkeeping.
  private clientSparse: Record<string, unknown> = {};
  private coreSparse: Record<string, unknown> = {};
  // The core-confirmed sparse state. Each flush PATCHes
  // diff(syncedCoreSparse, coreSparse): changed keys as values, removed keys
  // as the null reset sentinel, so reverted keys are deleted core-side.
  private syncedCoreSparse: Record<string, unknown> = {};
  // Core-destination keys edited before the baseline loaded; flushed as one
  // PATCH right after it lands. The baseline skips these so a queued edit is
  // never clobbered.
  private pendingCoreEdits = new Set<string>();

  // Secret keys the user actually edited this session. Only these are written
  // on save. A loaded-but-untouched secret field is empty (we never receive
  // the value), so without this guard an unrelated save would delete the
  // configured vault entry.
  private dirtySecrets = new Set<string>();

  private listeners = new Set<SettingChangeListener>();

  // Coalesced flush state: `pendingPrev` records the *first* observed prev
  // value per key across a debounce window so a failed flush can roll the
  // affected destination back to where it started. Resolvers are notified per
  // individual updateSettings() call.
  private pendingPrev = new Map<string, unknown>();
  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private pendingResolvers: Array<{
    resolve: () => void;
    reject: (e: unknown) => void;
  }> = [];
  private flushInFlight: Promise<void> | null = null;

  // attach() wiring guards: one-shot registration, plus the selected-core id
  // so registry notifications (which also fire on rename) only reset core
  // state on a real switch. baselineGen discards stale baseline GETs that
  // resolve after a newer load or a core switch started.
  private attached = false;
  private lastCoreId: string | null = null;
  private baselineGen = 0;

  /** True if the core reports a value stored for this secret-typed setting. */
  isSecretConfigured(key: string): boolean {
    return this.configuredSecrets.has(key);
  }

  onChange(fn: SettingChangeListener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  // --- the pipeline --------------------------------------------------------

  /** The one funnel every mutation goes through. A `null`/`undefined` value
   *  means "revert to schema default". Updates the owning sparse layer,
   *  mutates the merged record per key, and notifies listeners for keys whose
   *  effective value actually changed. Returns those transitions. */
  private applyChanges(
    changes: Record<string, unknown>,
    origin: SettingsChangeOrigin,
  ): Transition[] {
    const transitions: Transition[] = [];
    for (const [key, raw] of Object.entries(changes)) {
      if (!(key in DEFAULTS)) {
        // Render-only fields and junk keys hold no persistable value.
        if (dev) log.warn(`ignoring unknown setting key: "${key}"`);
        continue;
      }
      const next = raw === null || raw === undefined ? DEFAULTS[key] : raw;
      if (!SECRET_KEY_SET.has(key)) {
        // Secrets never enter the layers (their values live in the vault and
        // their UI value is session-local); everything else lands in its
        // destination's sparse layer, kept sparse against the defaults. Both
        // core-stored layers (core, client-on-core) ride coreSparse; only the
        // local client file uses clientSparse.
        const layer = destinationNeedsCore(destinationFor(key))
          ? this.coreSparse
          : this.clientSparse;
        if (Object.is(next, DEFAULTS[key])) delete layer[key];
        else layer[key] = next;
      }
      const prev = this.currentSettings[key];
      if (Object.is(prev, next)) continue;
      this.currentSettings[key] = next;
      transitions.push({ key, prev, next });
    }
    for (const t of transitions) {
      this.notifyListeners(t.key, t.prev, t.next, origin);
    }
    return transitions;
  }

  private notifyListeners(
    key: string,
    prev: unknown,
    next: unknown,
    origin: SettingsChangeOrigin,
  ): void {
    for (const fn of this.listeners) {
      try {
        void Promise.resolve(fn(key, prev, next, origin)).catch((e) =>
          log.warn(`onChange listener for "${key}" failed:`, e),
        );
      } catch (e) {
        log.warn(`onChange listener for "${key}" failed:`, e);
      }
    }
  }

  /** Replace the core layer with `sparse`: new/changed entries apply as
   *  values, entries no longer present revert to default. `skipKeys` protects
   *  locally-pending edits from being clobbered by a baseline. */
  private replaceCoreLayer(
    sparse: Record<string, unknown>,
    origin: SettingsChangeOrigin,
    skipKeys?: ReadonlySet<string>,
  ): void {
    const changes: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(sparse)) {
      if (!skipKeys?.has(k)) changes[k] = v;
    }
    for (const k of Object.keys(this.coreSparse)) {
      if (!(k in sparse) && !skipKeys?.has(k)) changes[k] = null;
    }
    this.applyChanges(changes, origin);
  }

  /** Client-layer counterpart of replaceCoreLayer (no skip set: the client
   *  file is read only at boot, before any edit can be pending). */
  private replaceClientLayer(sparse: Record<string, unknown>, origin: SettingsChangeOrigin): void {
    const changes: Record<string, unknown> = { ...sparse };
    for (const k of Object.keys(this.clientSparse)) {
      if (!(k in sparse)) changes[k] = null;
    }
    this.applyChanges(changes, origin);
  }

  // --- wiring ---------------------------------------------------------------

  /** One-shot wiring of the live-sync hooks. Called once from the deferred
   *  boot phase; everything afterwards (pairing, core switch, reconnect,
   *  remote settings changes) flows through these subscriptions instead of
   *  per-call-site loads. */
  attach(): void {
    if (this.attached) return;
    this.attached = true;
    this.lastCoreId = cores().currentEntry()?.id ?? null;

    // Selection changes: the registry notifies on select/remove/rename with
    // no payload, so compare ids to act only on a real switch. Resetting the
    // core layer fires real transitions (a switched-away core's tts.enabled
    // true reverts to false, disarming TTS) before the new baseline loads.
    cores().subscribe(() => {
      const id = cores().currentEntry()?.id ?? null;
      if (id === this.lastCoreId) return;
      this.lastCoreId = id;
      this.resetCoreState();
      if (id) {
        this.loadCoreSettings().catch((e) =>
          log.warn("core settings load after core switch failed:", e),
        );
      }
    });

    // Remote deltas: another client's PATCH (or a core-side change like a
    // preset apply) lands here; our own PATCH echoes back and value-diffs to
    // a no-op.
    cores().subscribeWs((frame) => {
      if (frame.kind !== "settings.updated") return;
      this.applyRemote(frame.values, frame.deleted, frame.secretNames);
    });

    // Reconnects: frames sent while disconnected are gone, so re-baseline on
    // every connected edge (first connect, post-pairing, core restart).
    cores().subscribeConnectionState((s) => {
      if (s !== "connected") return;
      this.loadCoreSettings().catch((e) => log.warn("core settings load on reconnect failed:", e));
    });

    // A core may already be selected (and even connected) by the time the
    // deferred boot phase attaches; load its baseline explicitly.
    if (cores().currentEntry()) {
      this.loadCoreSettings().catch((e) => log.warn("core settings baseline load failed:", e));
    }
  }

  private resetCoreState(): void {
    this.coreLoaded = false;
    this.pendingCoreEdits.clear();
    this.dirtySecrets.clear();
    this.configuredSecrets = new Set();
    this.syncedCoreSparse = {};
    this.baselineGen++;
    this.replaceCoreLayer({}, "load");
  }

  private applyRemote(
    values: Record<string, unknown>,
    deleted: string[],
    secretNames?: string[],
  ): void {
    const changes: Record<string, unknown> = {};
    const pending = (k: string) => this.pendingCoreEdits.has(k) || this.pendingPrev.has(k);
    for (const [k, v] of Object.entries(values)) {
      if (!isCoreKey(k)) continue;
      // The core's authoritative state moved either way; a key with a local
      // edit in flight keeps its optimistic merged value, and the next flush
      // diff re-asserts it against this new synced state.
      this.syncedCoreSparse[k] = v;
      if (!pending(k)) changes[k] = v;
    }
    for (const k of deleted) {
      if (!isCoreKey(k)) continue;
      delete this.syncedCoreSparse[k];
      if (!pending(k)) changes[k] = null;
    }
    this.applyChanges(changes, "remote");
    if (secretNames) this.configuredSecrets = new Set(secretNames);
  }

  // --- loads ----------------------------------------------------------------

  /** Raise platform-specific defaults before the first settings load. Two cases:
   *  - Mobile reads at a larger base text size (18px vs the desktop 16).
   *  - Windows global-shortcut defaults must not lead with `super`: that maps to
   *    the OS-reserved Win key, which `RegisterHotKey` accepts but Windows then
   *    silently swallows, so the default hotkeys never fire. Use `super`-free
   *    combos there instead (mac/Linux keep `super` = Cmd, which works).
   *  These are REAL defaults, not post-load overrides: the sparse layer treats
   *  them as the platform baseline, so a user who picks another value has it
   *  persisted like any non-default. Idempotent; called once from the layout boot
   *  with the resolved platform, before loadClientSettings merges the stored file
   *  over these defaults. */
  setPlatformDefaults(mobile: boolean, windows: boolean): void {
    if (mobile) DEFAULTS["appearance.textSize"] = 18;
    if (windows) {
      DEFAULTS["shortcuts.toggleWindow"] = "ctrl+alt+shift+z";
      DEFAULTS["shortcuts.attachFile"] = "ctrl+alt+shift+a";
      DEFAULTS["shortcuts.captureScreen"] = "ctrl+alt+shift+s";
      DEFAULTS["shortcuts.captureRegion"] = "ctrl+alt+shift+x";
    }
    // Reflect into the live merged view unless the user already overrode it, so
    // the first paint (before loadClientSettings rebuilds the merge) is correct.
    for (const key of [
      "appearance.textSize",
      "shortcuts.toggleWindow",
      "shortcuts.attachFile",
      "shortcuts.captureScreen",
      "shortcuts.captureRegion",
    ]) {
      if (!(key in this.clientSparse)) this.currentSettings[key] = DEFAULTS[key];
    }
  }

  /** Local-only load: defaults + the client settings file. Fast, no network.
   *  This is all the boot path needs before it can position, theme, and show
   *  the window. Core state is untouched; the attach() hooks own it. */
  async loadClientSettings(): Promise<void> {
    if (!browser) return;
    let stored: Record<string, unknown> = {};
    try {
      stored = await platform().clientFiles.read("settings");
    } catch (e) {
      log.warn("Failed to load client settings, using defaults:", e);
    }
    const sparse: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(stored)) {
      if (k in DEFAULTS && !SECRET_KEY_SET.has(k) && destinationFor(k) === "client-on-client") {
        sparse[k] = v;
      } else if (dev) {
        log.warn(`dropping non-client key from settings.json: "${k}"`);
      }
    }
    this.replaceClientLayer(sparse, "load");

    // Push the persisted shortcut so Rust overrides the startup default. Local
    // Rust call; boot must not abort if the shortcut is taken. Log it and let
    // Settings fix.
    this.applyToggleWindowShortcut(this.currentSettings["shortcuts.toggleWindow"]).catch((e) =>
      log.warn("Failed to register persisted shortcut:", e),
    );
    this.applyPttConfig().catch((e) => log.warn("Failed to sync PTT config:", e));
  }

  /** Fetch the selected core's settings baseline (and configured-secret
   *  names) and merge it through the pipeline. No-op when no core is
   *  selected; stale responses (a newer load started, or the core switched)
   *  are discarded. Public for callers that change core state server-side and
   *  want the merged view refreshed deterministically (model preset applies);
   *  the WS echo of such changes value-diffs to a no-op afterwards. */
  async loadCoreSettings(): Promise<void> {
    if (!browser) return;
    const entry = cores().currentEntry();
    if (!entry) return;
    const gen = ++this.baselineGen;
    const api = cores().api().settings;
    const [coreStored, names] = await Promise.all([api.load(), api.listSecrets()]);
    if (gen !== this.baselineGen || cores().currentEntry()?.id !== entry.id) {
      return;
    }

    const baseline: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(coreStored)) {
      if (isCoreKey(k)) baseline[k] = v;
    }
    // Keys with local edits in flight keep their optimistic value; the synced
    // state below still records the core's truth, so the queued flush diff
    // re-asserts the local value (and a failed flush rolls back to the core's
    // real value, which pendingPrev is re-pointed at here).
    const skip = new Set<string>(this.pendingCoreEdits);
    for (const k of this.pendingPrev.keys()) {
      if (isCoreKey(k)) skip.add(k);
    }
    this.replaceCoreLayer(baseline, "load", skip);
    this.syncedCoreSparse = { ...baseline };
    for (const k of skip) {
      if (this.pendingPrev.has(k)) {
        this.pendingPrev.set(k, k in baseline ? baseline[k] : undefined);
      }
    }
    this.configuredSecrets = new Set(names);
    this.coreLoaded = true;
    if (this.pendingCoreEdits.size > 0) {
      this.scheduleFlush().catch((e) => log.warn("queued core settings flush failed:", e));
    }
    // Align the thinking budget to the context window on first load. Only when
    // the core has never stored one (still the context-blind schema default):
    // a preset or a manual edit is a deliberate value we must not touch. Later
    // context/preset changes realign through their own paths.
    if (!("llm.reasoningBudget" in this.syncedCoreSparse)) {
      const aligned = initialThinkingBudgetAlignment(this.currentSettings);
      if (aligned !== null) {
        this.updateSetting("llm.reasoningBudget", aligned).catch((e) =>
          log.warn("initial thinking-budget alignment failed:", e),
        );
      }
    }
  }

  /** Local load then core baseline. Used after a storage-level settings
   *  reset; boot uses the split methods + attach() instead. */
  async loadSettings(): Promise<void> {
    await this.loadClientSettings();
    try {
      await this.loadCoreSettings();
    } catch (e) {
      log.warn("Failed to load core settings, falling back:", e);
    }
  }

  private async applyToggleWindowShortcut(value: unknown): Promise<void> {
    const accelerator = typeof value === "string" && value.length > 0 ? value : null;
    await platform().shortcuts.setBinding(accelerator);
  }

  /** Push the window-shortcut activation mode + hold duration to Rust, which
   *  owns the tap-vs-hold timing (see shortcut.svelte.ts). Call whenever
   *  `stt.activation` or `stt.holdDuration` changes so Rust classifies the next
   *  press correctly. */
  async applyPttConfig(): Promise<void> {
    const pushToTalk = this.currentSettings["stt.activation"] === "push-to-talk";
    const holdMs = Number(this.currentSettings["stt.holdDuration"]) || 250;
    await platform().shortcuts.setPttConfig(pushToTalk, holdMs);
  }

  // --- edits ----------------------------------------------------------------

  async updateSetting(key: string, value: unknown): Promise<void> {
    return await this.updateSettings({ [key]: value });
  }

  async updateSettings(updates: Record<string, unknown>): Promise<void> {
    // Shortcut bindings are validated against the OS BEFORE anything is
    // applied, so a taken combo throws without touching state.
    if ("shortcuts.toggleWindow" in updates) {
      await this.applyToggleWindowShortcut(updates["shortcuts.toggleWindow"]);
    }
    for (const key of [
      "shortcuts.attachFile",
      "shortcuts.captureScreen",
      "shortcuts.captureRegion",
    ]) {
      const value = updates[key];
      if (typeof value === "string" && value.trim().length > 0) {
        // Probe-validate the combo before persisting. Re-registration happens
        // when UserInput remounts; this just surfaces "already taken" so the
        // bad value doesn't get saved.
        await platform().shortcuts.validate(value);
      }
    }

    const transitions = this.applyChanges(updates, "user");

    // Keep Rust's tap-vs-hold config in step with the activation mode + hold
    // duration (applyChanges has already updated currentSettings).
    if ("stt.activation" in updates || "stt.holdDuration" in updates) {
      this.applyPttConfig().catch((e) => log.warn("Failed to sync PTT config:", e));
    }

    for (const t of transitions) {
      // First observed prev per key inside the debounce window, so rapid
      // A→B→C edits roll back to A on failure.
      if (!this.pendingPrev.has(t.key)) this.pendingPrev.set(t.key, t.prev);
      // Core edits before the baseline (mid-pairing, core unreachable) queue
      // rather than drop; loadCoreSettings flushes the queue once the
      // baseline lands. Only real transitions queue: a no-op write must not
      // shadow the key from the incoming baseline.
      if (isCoreKey(t.key) && !this.coreLoaded) {
        this.pendingCoreEdits.add(t.key);
      }
    }
    for (const key of Object.keys(updates)) {
      // Record explicit user edits to secret fields so save() writes only the
      // ones actually touched (an untouched secret field is empty because its
      // value is never returned, and must not clobber the vault entry).
      if (key in DEFAULTS && SECRET_KEY_SET.has(key)) {
        this.dirtySecrets.add(key);
      }
    }

    return await this.scheduleFlush();
  }

  /** Debounced flush scheduler. Each call resets the timer so rapid
   *  successive updates collapse into a single round-trip; the returned
   *  promise resolves (or rejects) when that flush completes. */
  private scheduleFlush(): Promise<void> {
    if (this.flushTimer !== null) clearTimeout(this.flushTimer);
    const p = new Promise<void>((resolve, reject) => {
      this.pendingResolvers.push({ resolve, reject });
    });
    this.flushTimer = setTimeout(() => {
      this.flushTimer = null;
      void this.flush();
    }, FLUSH_DEBOUNCE_MS);
    return p;
  }

  private async flush(): Promise<void> {
    // If a flush is already running, wait for it to finish before kicking
    // off the next one so we don't fire overlapping PATCHes to core.
    if (this.flushInFlight) {
      await this.flushInFlight.catch(() => {});
    }
    const prevSnapshot = new Map(this.pendingPrev);
    const resolvers = this.pendingResolvers.splice(0);
    this.pendingPrev.clear();

    const run = (async () => {
      const { errors, rollbackKeys } = await this.save();
      if (errors.length === 0) {
        for (const r of resolvers) r.resolve();
        return;
      }
      // Per-destination rollback: only the keys whose destination failed
      // revert (firing reverse transitions through the pipeline); the
      // destinations that persisted keep their values.
      const rollback: Record<string, unknown> = {};
      for (const [key, prev] of prevSnapshot) {
        if (rollbackKeys.has(key)) rollback[key] = prev;
      }
      this.applyChanges(rollback, "user");
      const error = new AggregateError(errors, "settings save failed");
      for (const r of resolvers) r.reject(error);
    })();
    this.flushInFlight = run;
    try {
      await run;
    } finally {
      this.flushInFlight = null;
    }
  }

  /** Persist each destination's sparse state. Failures are collected (never
   *  thrown) together with the set of keys the caller should roll back. */
  private async save(): Promise<{ errors: unknown[]; rollbackKeys: Set<string> }> {
    const errors: unknown[] = [];
    const rollbackKeys = new Set<string>();
    if (!browser) return { errors, rollbackKeys };

    // Client file: this store is its single owner, so a full write of the
    // sparse layer IS the file.
    try {
      await platform().clientFiles.write("settings", { ...this.clientSparse });
    } catch (e) {
      log.warn("Failed to save client settings:", e);
      errors.push(e);
      for (const key of Object.keys(DEFAULTS)) {
        if (!SECRET_KEY_SET.has(key) && destinationFor(key) === "client-on-client") {
          rollbackKeys.add(key);
        }
      }
    }

    if (cores().currentEntry()) {
      const api = cores().api().settings;
      // Core PATCH: the diff against the last core-confirmed state, with the
      // null reset sentinel for keys that reverted to default. Held while the
      // baseline is unknown (edits stay queued in pendingCoreEdits).
      if (this.coreLoaded) {
        const corePatch: Record<string, unknown> = {};
        for (const [k, v] of Object.entries(this.coreSparse)) {
          if (!(k in this.syncedCoreSparse) || !Object.is(this.syncedCoreSparse[k], v)) {
            corePatch[k] = v;
          }
        }
        for (const k of Object.keys(this.syncedCoreSparse)) {
          if (!(k in this.coreSparse)) corePatch[k] = null;
        }
        if (Object.keys(corePatch).length > 0) {
          try {
            // The response body is ignored: the WS echo carries the same
            // delta and value-diffs to a no-op.
            await api.patch(corePatch);
            this.syncedCoreSparse = { ...this.coreSparse };
            this.pendingCoreEdits.clear();
          } catch (e) {
            log.warn("Failed to save core settings:", e);
            errors.push(e);
            for (const k of Object.keys(corePatch)) rollbackKeys.add(k);
            // The synced state may have drifted (e.g. a remote delta skipped
            // for a then-pending key); one re-baseline heals it.
            this.loadCoreSettings().catch(() => {});
          }
        }
      }

      // Touched secrets only: a non-empty value sets the vault entry; an
      // emptied one clears it (the user explicitly deleted it).
      const nextConfigured = new Set(this.configuredSecrets);
      const persisted: string[] = [];
      for (const name of this.dirtySecrets) {
        const raw = this.currentSettings[name];
        const value = typeof raw === "string" ? raw : "";
        try {
          if (value === "") {
            await api.deleteSecret(name);
            nextConfigured.delete(name);
          } else {
            await api.setSecret(name, value);
            nextConfigured.add(name);
          }
          persisted.push(name);
        } catch (e) {
          log.warn(`Failed to update secret "${name}":`, e);
          errors.push(e);
          rollbackKeys.add(name);
        }
      }
      // Reassign for Svelte reactivity, and forget the edits we committed.
      this.configuredSecrets = nextConfigured;
      for (const name of persisted) this.dirtySecrets.delete(name);
    }

    return { errors, rollbackKeys };
  }

  getAlignment(): Alignment {
    return (this.currentSettings["layout.alignment"] as Alignment) ?? "center";
  }

  getMonitor(): string {
    return this.currentSettings["layout.monitor"]?.toString() || "primary";
  }
}

// Re-exported for downstream consumers that want to introspect destinations.
export { isClientGroup, isCoreGroup, type SettingGroupId };

export const settingsState = new SettingsState();
