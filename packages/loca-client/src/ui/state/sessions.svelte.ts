/**
 * Session metadata, list, and cross-slice orchestration. Owns the active
 * session id, its display title, the list index, and an epoch counter that
 * lets components remount cleanly on session boundaries.
 *
 * All session persistence is now server-side: list/load/save/delete go
 * through cores().api().sessions. Title generation runs server-side too;
 * the client just receives `session.updated` frames.
 */

import type { Message as ServerMessage, ServerToClientFrame, SessionListEntry } from "@loca/shared";
import type { Message } from "$lib/util/types";
import { cores } from "$lib/core";
import { formatSessionDefaultTitle } from "$lib/util/format";
import { platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { Subscriptions } from "$lib/util/subscriptions";
import { messagesState } from "./messages.svelte";
import { settingsState } from "./settings.svelte";
import { streamingState } from "./streaming.svelte";
import { viewState } from "./view.svelte";

const log = getLogger("sessions");

export type SessionInfo = SessionListEntry;

/** Defensive snapshot fixups applied on disk → memory load. The persisted
 *  shape IS the render shape now, so this only repairs in-flight state a
 *  crash or disconnect froze mid-turn: ids missing from pre-id files, tool
 *  calls stuck non-terminal. */
export function fixupLoadedMessages(messages: ServerMessage[]): Message[] {
  let counter = 0;
  // Each wire message widens into the client bag (every wire field is an
  // optional bag field); copy per element rather than reinterpreting the array.
  const out: Message[] = messages.map((m) => ({ ...m }));
  for (let i = 0; i < out.length; i++) {
    const m = out[i];
    if (!m.id) m.id = `load-${Date.now()}-${counter++}`;
    if (
      m.role === "tool" &&
      m.status !== "completed" &&
      m.status !== "failed" &&
      m.status !== "cancelled"
    ) {
      out[i] = {
        ...m,
        status: "failed",
        error: m.error ?? "interrupted: Core was disconnected mid-call",
      };
    }
  }
  return out;
}

class SessionsState {
  list = $state<SessionInfo[]>([]);
  id = $state<string | null>(null);
  title = $state<string>("");
  /** True while core is (re)generating this session's title. Driven by
   *  `title_generating` frames; set optimistically on manual regenerate. */
  generatingTitle = $state(false);
  /** Creation timestamp of the active session, from the server's session
   *  doc. Drives `defaultTitle`; session ids are opaque ULIDs. */
  createdAtMs = $state<number | null>(null);
  /** Whether the active session is temporary (RAM-only on the core: never
   *  listed, never on disk, gone once left). Fixed at creation; while a fresh
   *  chat is unstarted this doubles as the "compose temporary" intent that the
   *  next session is minted with. */
  isTemporary = $state(false);
  currentIndex = $state<number>(-1);
  /** Bumps on every session-boundary transition. Consumers use it as a
   *  `{#key}` to force a clean DOM teardown of the message subtree. */
  epoch = $state(0);

  private subs = new Subscriptions();
  // Session whose finished stream should pop the window (a greeting with
  // focus "show_when_done").
  private showWhenDoneId: string | null = null;
  // True while recoverFromFailedLoad runs, so a failing recovery target
  // doesn't recurse into another recovery.
  private recovering = false;
  // Reconnect-resync bookkeeping: only reload after a real gap (a drop following
  // a prior connect), never on the very first connect of the session.
  private hasConnected = false;
  private droppedSinceConnected = false;

  get storageEnabled(): boolean {
    // Whether new sessions are persisted by default. When off, "don't persist"
    // is realized through the SAME mechanism as the per-chat temporary toggle:
    // new sessions are minted temporary (RAM-only on the core), so there is one
    // backend for "not saved", not a separate client-side suppression path.
    return settingsState.currentSettings["general.session.storeSessions"] !== false;
  }

  /** Formatted creation datetime of the active session. Shown as the title
   *  placeholder and filled in whenever the user clears the title. */
  get defaultTitle(): string {
    if (this.createdAtMs == null) return "";
    return formatSessionDefaultTitle(this.createdAtMs);
  }

  /** Whether the active conversation has begun: it has a message or a live
   *  stream. Drives the composer's temporary toggle, which is only offered
   *  while a chat is still unstarted (its class is fixed once it starts). */
  get started(): boolean {
    return this.id != null && (messagesState.messages.length > 0 || streamingState.isActive);
  }

  /** Wire up to session.updated frames so title changes / server-side
   *  message persistence reach the UI without polling. */
  attach(): void {
    this.subs.attach(() => [
      cores().subscribeWs((frame: ServerToClientFrame) => {
        if (frame.kind === "session.created") {
          void this.onSessionCreated(frame);
          return;
        }
        if (frame.kind !== "session.updated") return;
        if (frame.sessionId !== this.id) return;
        if (frame.op === "title_generating") {
          this.generatingTitle = frame.payload?.generating ?? false;
        } else if (frame.op === "title_changed" && frame.payload?.title !== undefined) {
          this.title = frame.payload.title;
          this.generatingTitle = false;
          const idx = this.list.findIndex((s) => s.id === this.id);
          if (idx >= 0) {
            this.list[idx] = { ...this.list[idx], title: frame.payload.title };
          }
        } else if (
          (frame.op === "message_added" || frame.op === "message_updated") &&
          frame.payload?.message
        ) {
          // REST-driven changes (another paired client's message or edit).
          // Streamed chat messages arrive via chat.message instead.
          messagesState.applyServerMessage(frame.payload.message as ServerMessage, null);
        } else if (frame.op === "message_deleted" && frame.payload?.messageId) {
          messagesState.removeById(frame.payload.messageId);
        }
      }),
      // During a disconnect gap the session.updated/message_added frames above are
      // lost, so server-side changes (e.g. a finished turn, a session created or
      // deleted by another client) are missed. On reconnect, refetch the session
      // list and reload the active session to resync; load() also re-runs the
      // in-flight fixups in backfillMessageIds. Without the list refetch the chat
      // bar's prev/next/list controls keep a stale (or empty) list and read as
      // "no sessions". Only after a real gap, never on first connect.
      //
      // This connected-edge load() is also what recovers a chat.subscribe that
      // load() sent into a not-yet-open socket (sendWs drops silently then): right
      // after a core swap the new socket is still "connecting", so the immediate
      // loadLatest()->load() subscribe is dropped; this re-fires it once the
      // socket opens, so an in-flight turn on the freshly-selected core resumes.
      // The "connecting" churn of a swap sets droppedSinceConnected because
      // hasConnected was already true from the prior core.
      cores().subscribeConnectionState((state) => {
        if (state === "connected") {
          if (this.droppedSinceConnected) {
            log.info("reconnected; reloading session list");
            void this.loadList();
            if (this.id) {
              log.info("reconnected; reloading active session");
              void this.load(this.id);
            }
          }
          this.hasConnected = true;
          this.droppedSinceConnected = false;
        } else if (this.hasConnected) {
          // Disconnected/connecting after a prior connect: a real gap to resync.
          this.droppedSinceConnected = true;
        }
      }),
    ]);
  }

  /** Core created a session on its own (a scheduled prompt or greeting
   *  fired). Surface it per the frame's focus: "show" pops the window now,
   *  "show_when_done" pops it when the session's stream finishes.
   *
   *  The window-reveal contract is established BEFORE the awaits below: a
   *  fast (or fast-failing) stream can reach chat.done/chat.error, which
   *  calls notifyStreamDone, before loadList resolves, and it must find the
   *  latch already armed. It is also independent of the user's in-flight
   *  work: an automated background stream counts as active work but leaves
   *  the window hidden, so a "show" must still reveal it. Only navigation is
   *  skipped while the user is busy, since load() would interrupt them; the
   *  session still appears in the refreshed list. */
  private async onSessionCreated(
    frame: Extract<ServerToClientFrame, { kind: "session.created" }>,
  ): Promise<void> {
    if (frame.focus === "show") {
      void platform().windowing.show();
    } else {
      this.showWhenDoneId = frame.session.id;
    }
    await this.loadList();
    if (streamingState.hasActiveWork) return;
    await this.load(frame.session.id);
    viewState.navigate("chat");
  }

  /** Called by streaming when a stream's chat.done lands; pops the window
   *  for a session created with focus "show_when_done". */
  notifyStreamDone(sessionId: string | null): void {
    if (sessionId !== null && this.showWhenDoneId === sessionId) {
      this.showWhenDoneId = null;
      void platform().windowing.show();
    }
  }

  detach(): void {
    this.subs.detach();
    this.hasConnected = false;
    this.droppedSinceConnected = false;
  }

  async loadList(): Promise<void> {
    // No storage-off special case: when storage is off, new sessions are
    // temporary and the core already omits them from this list, so the list is
    // naturally empty without a separate client-side suppression.
    try {
      this.list = await cores().api().sessions.list();
      if (this.id) {
        this.currentIndex = this.list.findIndex((s) => s.id === this.id);
      }
    } catch (e) {
      log.error("Failed to load session list:", e);
    }
  }

  async updateTitle(title: string): Promise<void> {
    this.title = title;
    if (!this.id) return;
    try {
      await cores().api().sessions.patchTitle(this.id, title);
      const idx = this.list.findIndex((s) => s.id === this.id);
      if (idx >= 0) this.list[idx] = { ...this.list[idx], title };
    } catch (e) {
      log.error("Failed to save session title:", e);
    }
  }

  /** Ask core to regenerate the active session's title. Core drives the
   *  spinner via `title_generating` frames; we set it optimistically so the
   *  click feels instant, and clear it on error since no frame will arrive. */
  async regenerateTitle(): Promise<void> {
    if (!this.id || this.generatingTitle) return;
    this.generatingTitle = true;
    try {
      await cores().api().sessions.regenerateTitle(this.id);
    } catch (e) {
      log.error("Failed to regenerate session title:", e);
      this.generatingTitle = false;
    }
  }

  /** Reset every in-memory field that's tied to a single session. */
  private resetAll(): void {
    messagesState.clear();
    streamingState.resetForSession();
    this.id = null;
    this.title = "";
    this.generatingTitle = false;
    this.createdAtMs = null;
    // Default the fresh compose state's class to the storage setting: storage
    // off means the next session is temporary unless something loads over it.
    this.isTemporary = !this.storageEnabled;
    this.epoch++;
  }

  async loadLatest(): Promise<void> {
    try {
      await this.loadList();
      // With storage off the list is empty (new sessions are temporary), so
      // this naturally lands on a fresh compose instead of reopening anything.
      if (this.list.length === 0) return;
      const first = this.list[0];
      await this.load(first.id);
    } catch (e) {
      log.error("Failed to load latest:", e);
    }
  }

  async load(sessionId: string): Promise<void> {
    // Leaving a temporary session for a different one discards it (RAM-only,
    // gone the moment you navigate away). Skip when reloading the same id (the
    // reconnect resync re-fetches the live temporary session).
    if (sessionId !== this.id) await this.discardActiveTemporary();
    await streamingState.interruptStreaming();
    streamingState.resetTTSPlayback();
    try {
      const full = await cores().api().sessions.get(sessionId);
      this.resetAll();
      // The server stores messages oldest-first (ascending ord); the
      // transcript array is newest-first.
      messagesState.hydrate(fixupLoadedMessages(full.messages).reverse(), full.tokenUsage ?? null);
      this.id = full.id;
      this.createdAtMs = full.createdAtMs ?? null;
      this.isTemporary = full.temporary ?? false;
      this.title = full.title || this.defaultTitle;
      this.currentIndex = this.list.findIndex((s) => s.id === sessionId);
      // Re-attach to any turn still generating on this session: the core
      // re-emits the in-flight messages so far (catch-up) plus any open tool
      // prompt, then live deltas resume. A no-op server-side when nothing is in
      // flight. Covers reopening after a core swap and the reconnect resync; the
      // GET above only returns finalized messages, so this fills the live tail.
      cores().api().chat.subscribe(full.id);
    } catch (e) {
      log.error("Failed to load session:", e);
      await this.recoverFromFailedLoad(sessionId);
    }
  }

  /** A session fetch can fail because the requested id is stale: the session
   *  was deleted by another client, from the Settings storage view, or
   *  between a disconnect and the reconnect resync. Instead of wedging on
   *  the dead id (an unloadable session with a hidden session bar), drop the
   *  local state tied to it, refresh the list from the server, and open the
   *  newest remaining session. */
  private async recoverFromFailedLoad(sessionId: string): Promise<void> {
    if (this.recovering) return;
    this.recovering = true;
    try {
      if (this.id === sessionId) this.resetAll();
      await this.loadList();
      const next = this.list[0];
      if (next && next.id !== sessionId) await this.load(next.id);
    } catch (e) {
      log.warn("session recovery failed:", e);
    } finally {
      this.recovering = false;
    }
  }

  /** Delete an arbitrary session by id. When it's the active session this
   *  delegates to delete() (session-boundary reset + navigation to a
   *  neighbour); otherwise it just drops the row from the list in place. */
  async deleteById(id: string): Promise<void> {
    if (id === this.id) {
      await this.delete();
      return;
    }
    try {
      await cores().api().sessions.delete(id);
    } catch (e) {
      log.error("Failed to delete session:", e);
      return;
    }
    this.list = this.list.filter((s) => s.id !== id);
    if (this.id) {
      this.currentIndex = this.list.findIndex((s) => s.id === this.id);
    }
  }

  async delete(): Promise<void> {
    if (!this.id) return;
    const sessionToDelete = this.id;
    const deletedIndex = this.list.findIndex((s) => s.id === sessionToDelete);
    const remaining = this.list.filter((s) => s.id !== sessionToDelete);

    await streamingState.interruptStreaming();
    streamingState.resetTTSPlayback();
    this.resetAll();

    try {
      await cores().api().sessions.delete(sessionToDelete);
    } catch (e) {
      log.error("Failed to delete session:", e);
    }
    this.list = remaining;

    if (remaining.length === 0) {
      this.currentIndex = 0;
      return;
    }
    const nextIdx =
      deletedIndex < 0 ? 0 : deletedIndex < remaining.length ? deletedIndex : remaining.length - 1;
    await this.load(remaining[nextIdx].id);
  }

  /** Enter "compose mode" for a fresh chat. The server session is NOT created
   *  here: the first user message mints it (messages.addUserMessage), with the
   *  class chosen by the composer toggle / storage setting. So "+" just clears
   *  to an empty composer, and an abandoned new chat leaves nothing behind. */
  async create(): Promise<void> {
    await this.discardActiveTemporary();
    await streamingState.interruptStreaming();
    streamingState.resetTTSPlayback();
    this.resetAll();
    this.currentIndex = -1;
    // Refresh the list so the bar reflects existing sessions; the new chat
    // itself appears only once its first message creates it.
    await this.loadList();
  }

  /** Flip the composer's temporary intent. Only reachable while the active
   *  chat is unstarted (the composer hides the toggle once it starts, since a
   *  session's class is fixed at creation). Normally no session exists yet, so
   *  this just records the intent for the lazy create. If an unstarted session
   *  does exist (e.g. an empty persistent one loaded from the list), discard it
   *  so the next message re-mints with the chosen class. */
  async toggleTemporary(): Promise<void> {
    const next = !this.isTemporary;
    if (this.id && !this.started) {
      const stray = this.id;
      this.resetAll();
      try {
        await cores().api().sessions.delete(stray);
      } catch (e) {
        log.warn("Failed to discard stray session on temporary toggle:", e);
      }
      void this.loadList();
    }
    this.isTemporary = next;
  }

  /** Delete the active session if it is temporary. Called before navigating
   *  away so a temporary (RAM-only) session never lingers on the core once it
   *  is no longer reachable. Best-effort: a failure must not block navigation. */
  private async discardActiveTemporary(): Promise<void> {
    if (!this.isTemporary || !this.id) return;
    const id = this.id;
    try {
      await cores().api().sessions.delete(id);
    } catch (e) {
      log.warn("Failed to discard temporary session:", e);
    }
  }
}

export const sessionsState = new SessionsState();
