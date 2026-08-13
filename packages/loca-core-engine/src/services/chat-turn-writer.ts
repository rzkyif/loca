// Owns one turn's message announcements and persistence. `born` announces a
// message to the client (chat.message, final: false) at the live insertion
// position; `finalize` persists it at the durable insertion cursor and emits
// the terminal snapshot. The two cursors only differ while messages of the
// same hop are live concurrently (reasoning + assistant), and converge
// because finalization happens in birth order (reasoning before assistant).

import type { Message, ServerToClientFrame } from "@loca/shared";
import { sessionsRepo } from "./sessions-store.ts";
import type { ActiveStream } from "./chat-types.ts";

export class TurnWriter {
  // Last persisted id; where the next finalize inserts.
  private cursor: string | null;
  // Last announced id (born or first-emission finalize); where the next
  // birth points its afterId.
  private liveCursor: string | null;
  private bornIds = new Set<string>();

  constructor(
    private readonly stream: ActiveStream,
    private readonly send: (clientId: string, frame: ServerToClientFrame) => void,
    anchorId: string | null,
  ) {
    this.cursor = anchorId;
    this.liveCursor = anchorId;
  }

  born(message: Message): void {
    // Buffer the live ref (in birth order) with the afterId it was born at, so
    // a mid-turn (re)subscribe can replay this born snapshot for catch-up.
    this.stream.liveMessages.set(message.id, { message, afterId: this.liveCursor });
    this.send(this.stream.clientId, {
      kind: "chat.message",
      streamId: this.stream.streamId,
      sessionId: this.stream.sessionId,
      message,
      afterId: this.liveCursor,
      final: false,
    });
    this.bornIds.add(message.id);
    this.liveCursor = message.id;
  }

  async finalize(message: Message): Promise<void> {
    // A message finalized without a prior birth (e.g. a tool-call-only
    // assistant that never streamed content) is positioned by this frame,
    // so it carries the live cursor as its afterId.
    const firstEmission = !this.bornIds.has(message.id);
    const { ord } = await sessionsRepo().insertMessageAfter(
      this.stream.sessionId,
      message,
      this.cursor,
    );
    message.ord = ord;
    this.cursor = message.id;
    // Now persisted (reloadable), so drop it from the catch-up buffer.
    this.stream.liveMessages.delete(message.id);
    this.send(this.stream.clientId, {
      kind: "chat.message",
      streamId: this.stream.streamId,
      sessionId: this.stream.sessionId,
      message,
      afterId: firstEmission ? this.liveCursor : null,
      final: true,
    });
    if (firstEmission) this.liveCursor = message.id;
  }
}
