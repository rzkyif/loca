// SessionsRepo CRUD + the cross-client tenancy guarantee that the owner checks
// enforce, plus the filesystem specifics (on-disk session.json, listAll,
// deleteById) and the per-session lock's lost-update safety. Message/attachment
// edge cases are covered by the chat.ts integration test. Keep this slim.

import { assertEquals, assertNotEquals, assertRejects, assertThrows } from "@std/assert";
import { join } from "@std/path";
import type { Message } from "@loca/shared";
import { sessionsRepo, sweepOrphanedSessionDirs } from "@loca/core-engine/services/sessions-store";
import { createTestClient, setupTestEnv } from "../../tests/helpers/db.ts";
import { paths } from "../paths.ts";
import { newMessageId } from "@loca/core-engine";
import { AppError } from "@loca/core-engine";

function userMessage(content = "hello"): Message {
  return {
    id: newMessageId(),
    ord: 0,
    role: "user",
    content,
    createdAtMs: Date.now(),
  };
}

Deno.test("SessionsRepo.create: returns session with id, timestamps, and default title", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const client = createTestClient();
    const s = await repo.create({ ownerClientId: client });
    assertEquals(s.ownerClientId, client);
    assertEquals(s.title, "");
    assertEquals(typeof s.id, "string");
    assertEquals(s.createdAtMs, s.updatedAtMs);
    // Persisted as a JSON file on disk.
    const onDisk = Deno.statSync(join(paths().sessionsDir, s.id, "session.json"));
    assertEquals(onDisk.isFile, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.list: only returns sessions owned by the caller", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const a = createTestClient("a");
    const b = createTestClient("b");
    await repo.create({ ownerClientId: a, title: "a-1" });
    await repo.create({ ownerClientId: a, title: "a-2" });
    await repo.create({ ownerClientId: b, title: "b-1" });
    const aList = await repo.list(a);
    const bList = await repo.list(b);
    assertEquals(aList.length, 2);
    assertEquals(bList.length, 1);
    assertEquals(bList[0].title, "b-1");
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.getOrThrow: rejects access from a non-owner client", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const intruder = createTestClient("intruder");
    const s = await repo.create({ ownerClientId: owner });
    await repo.getOrThrow(owner, s.id);
    await assertRejects(() => repo.getOrThrow(intruder, s.id), AppError);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.patchTitle: persists across reads and updates updatedAt", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    const before = (await repo.getOrThrow(owner, s.id)).updatedAtMs;
    await new Promise((r) => setTimeout(r, 5));
    await repo.patchTitle(owner, s.id, "renamed");
    const after = await repo.getOrThrow(owner, s.id);
    assertEquals(after.title, "renamed");
    assertNotEquals(before, after.updatedAtMs);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.delete: removes and rejects subsequent reads", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    const { attachmentPaths } = await repo.delete(owner, s.id);
    assertEquals(attachmentPaths, []);
    await assertRejects(() => repo.getOrThrow(owner, s.id), AppError);
    assertThrows(() => Deno.statSync(join(paths().sessionsDir, s.id)), Deno.errors.NotFound);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.delete: rejects when invoked from a non-owner client", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const intruder = createTestClient("intruder");
    const s = await repo.create({ ownerClientId: owner });
    await assertRejects(() => repo.delete(intruder, s.id), AppError);
    await repo.getOrThrow(owner, s.id);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.appendMessage + listMessages: round-trips message in order", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    await repo.appendMessage(s.id, userMessage("first"));
    await repo.appendMessage(s.id, userMessage("second"));
    const msgs = await repo.listMessages(s.id);
    assertEquals(msgs.length, 2);
    assertEquals(msgs[0].ord < msgs[1].ord, true);
    assertEquals((msgs[0] as { content: string }).content, "first");
    assertEquals((msgs[1] as { content: string }).content, "second");
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.insertMessageAfter: splices mid-history and renumbers ord", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    const first = await repo.appendMessage(s.id, userMessage("first"));
    await repo.appendMessage(s.id, userMessage("third"));
    await repo.insertMessageAfter(s.id, userMessage("second"), first.id);
    const msgs = await repo.listMessages(s.id);
    assertEquals(
      msgs.map((m) => (m as { content: string }).content),
      ["first", "second", "third"],
    );
    assertEquals(
      msgs.map((m) => m.ord),
      [0, 1, 2],
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.insertMessageAfter: null or unknown afterId appends at the tail", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    await repo.appendMessage(s.id, userMessage("first"));
    await repo.insertMessageAfter(s.id, userMessage("second"), null);
    await repo.insertMessageAfter(s.id, userMessage("third"), "missing-id");
    const msgs = await repo.listMessages(s.id);
    assertEquals(
      msgs.map((m) => (m as { content: string }).content),
      ["first", "second", "third"],
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.deleteTurn: removes between anchor and next user message", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    const anchor = await repo.appendMessage(s.id, userMessage("turn one"));
    const reply = await repo.appendMessage(s.id, {
      id: newMessageId(),
      ord: 0,
      role: "assistant",
      content: "reply one",
      createdAtMs: Date.now(),
    });
    await repo.appendMessage(s.id, userMessage("turn two"));
    await repo.appendMessage(s.id, {
      id: newMessageId(),
      ord: 0,
      role: "assistant",
      content: "reply two",
      createdAtMs: Date.now(),
    });

    const removed = await repo.deleteTurn(s.id, anchor.id);
    assertEquals(removed, [reply.id]);
    const msgs = await repo.listMessages(s.id);
    assertEquals(
      msgs.map((m) => (m as { content: string }).content),
      ["turn one", "turn two", "reply two"],
    );
    assertEquals(
      msgs.map((m) => m.ord),
      [0, 1, 2],
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.deleteTurn: newest turn, empty turn, and unknown anchor", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    await repo.appendMessage(s.id, userMessage("turn one"));
    const anchor = await repo.appendMessage(s.id, userMessage("turn two"));
    const reply = await repo.appendMessage(s.id, {
      id: newMessageId(),
      ord: 0,
      role: "assistant",
      content: "reply",
      createdAtMs: Date.now(),
    });

    // Newest turn: removes through the tail.
    assertEquals(await repo.deleteTurn(s.id, anchor.id), [reply.id]);
    // Now the anchor's turn is empty: no-op.
    assertEquals(await repo.deleteTurn(s.id, anchor.id), []);
    // Unknown anchor: no-op.
    assertEquals(await repo.deleteTurn(s.id, "missing-id"), []);
    assertEquals((await repo.listMessages(s.id)).length, 2);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo: deleting a session removes its messages", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });
    await repo.appendMessage(s.id, userMessage("doomed"));
    await repo.delete(owner, s.id);
    assertEquals(await repo.listMessages(s.id), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo: temporary session is RAM-only and excluded from every listing", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const persistent = await repo.create({ ownerClientId: owner, title: "kept" });
    const temp = await repo.create({ ownerClientId: owner, temporary: true });

    // The flag round-trips, and nothing was written to disk for it.
    assertEquals(temp.temporary, true);
    assertThrows(
      () => Deno.statSync(join(paths().sessionsDir, temp.id, "session.json")),
      Deno.errors.NotFound,
    );

    // CRUD works through the in-memory map.
    await repo.getOrThrow(owner, temp.id);
    await repo.appendMessage(temp.id, userMessage("secret"));
    assertEquals((await repo.listMessages(temp.id)).length, 1);
    await repo.patchTitle(owner, temp.id, "still hidden");
    assertEquals((await repo.getOrThrow(owner, temp.id)).title, "still hidden");

    // Excluded from the per-client list and the cross-client storage view.
    assertEquals(
      (await repo.list(owner)).map((s) => s.id),
      [persistent.id],
    );
    assertEquals(
      (await repo.listAll()).map((s) => s.id),
      [persistent.id],
    );

    // Deleting drops it from the map; subsequent reads fail.
    await repo.delete(owner, temp.id);
    await assertRejects(() => repo.getOrThrow(owner, temp.id), AppError);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.sweepClientTemporary: drops the caller's other temporary sessions only", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const a = createTestClient("a");
    const b = createTestClient("b");
    const keep = await repo.create({ ownerClientId: a, temporary: true });
    const orphan = await repo.create({ ownerClientId: a, temporary: true });
    const persistent = await repo.create({ ownerClientId: a, title: "kept" });
    const otherClientTemp = await repo.create({ ownerClientId: b, temporary: true });

    await repo.sweepClientTemporary(a, keep.id);

    // The kept session survives; the orphaned temporary one is gone.
    await repo.getOrThrow(a, keep.id);
    await assertRejects(() => repo.getOrThrow(a, orphan.id), AppError);
    // A persistent session of the same client is never swept.
    await repo.getOrThrow(a, persistent.id);
    // Another client's temporary session is out of scope.
    await repo.getOrThrow(b, otherClientTemp.id);
  } finally {
    await env.teardown();
  }
});

Deno.test("sweepOrphanedSessionDirs: removes attachments-only dirs, keeps real sessions", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const real = await repo.create({ ownerClientId: owner, title: "real" });

    // Simulate a temporary session's leftover attachment dir: a session
    // directory with no session.json (its doc lived only in RAM).
    const orphanDir = join(paths().sessionsDir, "orphan-temp", "attachments");
    Deno.mkdirSync(orphanDir, { recursive: true });
    Deno.writeTextFileSync(join(orphanDir, "file.bin"), "bytes");

    await sweepOrphanedSessionDirs();

    assertThrows(
      () => Deno.statSync(join(paths().sessionsDir, "orphan-temp")),
      Deno.errors.NotFound,
    );
    // The real session, which has a session.json, is untouched.
    assertEquals(Deno.statSync(join(paths().sessionsDir, real.id, "session.json")).isFile, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo.listAll: spans clients with on-disk sizes; deleteById is owner-agnostic", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const a = createTestClient("a");
    const b = createTestClient("b");
    const s1 = await repo.create({ ownerClientId: a, title: "a-1" });
    const s2 = await repo.create({ ownerClientId: b, title: "b-1" });
    await repo.appendMessage(s1.id, userMessage("hello"));

    const all = await repo.listAll();
    assertEquals(all.length, 2);
    const e1 = all.find((e) => e.id === s1.id)!;
    assertEquals(e1.ownerClientId, a);
    assertEquals(e1.sizeBytes > 0, true);

    // deleteById removes without an owner argument.
    const res = await repo.deleteById(s2.id);
    assertEquals(res?.ownerClientId, b);
    assertEquals((await repo.listAll()).length, 1);
    assertEquals(await repo.deleteById("nonexistent"), null);
  } finally {
    await env.teardown();
  }
});

Deno.test("SessionsRepo: concurrent appends on one session never lose an update (per-session lock)", async () => {
  const env = await setupTestEnv();
  try {
    const repo = sessionsRepo();
    const owner = createTestClient("owner");
    const s = await repo.create({ ownerClientId: owner });

    // Fire many appends at the SAME session concurrently. Each is a
    // read-modify-write of session.json with an await between read and write;
    // without the per-session lock they would interleave and clobber each
    // other, persisting far fewer than N messages. All must survive.
    const N = 40;
    await Promise.all(
      Array.from({ length: N }, (_, i) => repo.appendMessage(s.id, userMessage(`m${i}`))),
    );

    const msgs = await repo.listMessages(s.id);
    assertEquals(msgs.length, N);
    // ord values are a contiguous 0..N-1 with no gaps or dupes.
    assertEquals(
      msgs.map((m) => m.ord).sort((x, y) => x - y),
      Array.from({ length: N }, (_, i) => i),
    );
    // Every distinct content made it (no lost writes).
    assertEquals(new Set(msgs.map((m) => (m as { content: string }).content)).size, N);
  } finally {
    await env.teardown();
  }
});
