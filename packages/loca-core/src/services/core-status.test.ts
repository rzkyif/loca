// CoreStatusService: the priority order of the aggregate status, the
// per-subsystem fold (sidecars broken / loading), the busy queue counts, and the
// edge-only emit. The pushed signals (boot, streams, queues, update) need no
// sidecars; the sidecar source is injected so the error / loading fold is
// testable without spawning processes.

import { assertEquals } from "@std/assert";
import type { DownloadEntry, DownloadStatus, SidecarSnapshot } from "@loca/shared";
import { CoreStatusService } from "./core-status.ts";
import { __resetUpdateSubscribersForTesting, emitUpdate } from "../update/self-updater.ts";

function fresh(): CoreStatusService {
  __resetUpdateSubscribersForTesting();
  const svc = new CoreStatusService();
  svc.wire();
  return svc;
}

// A service with a mutable, injected sidecar source so a test can drive the
// per-subsystem fold (error / loading) without a real sidecar process.
function withSidecars(): { svc: CoreStatusService; set: (s: SidecarSnapshot[]) => void } {
  __resetUpdateSubscribersForTesting();
  let sidecars: SidecarSnapshot[] = [];
  const svc = new CoreStatusService(() => sidecars);
  svc.wire();
  return {
    svc,
    set: (s) => {
      sidecars = s;
      svc.recompute();
    },
  };
}

Deno.test("core-status: boots as starting_up until boot done", () => {
  const svc = fresh();
  assertEquals(svc.snapshot().status, "starting_up");
  svc.noteBootDone();
  assertEquals(svc.snapshot().status, "idle");
});

Deno.test("core-status: active streams and queues make it busy", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteActiveStreams(1);
  assertEquals(svc.snapshot().status, "busy");
  svc.noteActiveStreams(0);
  assertEquals(svc.snapshot().status, "idle");

  svc.noteLlmQueue(0, 2);
  assertEquals(svc.snapshot().status, "busy");
  assertEquals(svc.snapshot().detail, "2 queued");
  svc.noteLlmQueue(0, 0);
  assertEquals(svc.snapshot().status, "idle");

  svc.noteSpeechQueue(1, 0);
  assertEquals(svc.snapshot().status, "busy");
  svc.noteSpeechQueue(0, 0);
  assertEquals(svc.snapshot().status, "idle");
});

Deno.test("core-status: a staged update outranks busy", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteActiveStreams(1);
  assertEquals(svc.snapshot().status, "busy");
  emitUpdate({ kind: "staged", version: "9.9.9" });
  assertEquals(svc.snapshot().status, "updating");
});

Deno.test("core-status: starting_up (booting) outranks busy", () => {
  const svc = fresh();
  // still booting
  svc.noteActiveStreams(1);
  assertEquals(svc.snapshot().status, "starting_up");
});

Deno.test("core-status: a re-note of the same value does not emit", () => {
  const svc = fresh();
  svc.noteBootDone();
  const seen: string[] = [];
  svc.subscribe((s) => seen.push(s.status));
  svc.noteActiveStreams(1);
  svc.noteActiveStreams(1); // identical, the note guard short-circuits
  svc.noteActiveStreams(0);
  assertEquals(seen, ["busy", "idle"]);
});

Deno.test("core-status: busy snapshot carries the per-queue counts", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteLlmQueue(1, 3);
  svc.noteSpeechQueue(0, 2);
  svc.noteActiveStreams(2);
  const s = svc.snapshot();
  assertEquals(s.status, "busy");
  assertEquals(s.queues, {
    llmActive: 1,
    llmQueued: 3,
    speechActive: 0,
    speechQueued: 2,
    activeStreams: 2,
  });
  // Idle carries no queue block.
  svc.noteLlmQueue(0, 0);
  svc.noteSpeechQueue(0, 0);
  svc.noteActiveStreams(0);
  assertEquals(svc.snapshot().queues, undefined);
});

Deno.test("core-status: a changed queue count re-emits so the busy card stays live", () => {
  const svc = fresh();
  svc.noteBootDone();
  const seen: number[] = [];
  svc.subscribe((s) => seen.push(s.queues?.activeStreams ?? -1));
  svc.noteActiveStreams(1);
  svc.noteActiveStreams(2); // distinct count -> re-emits (was a no-op before)
  svc.noteActiveStreams(0); // back to idle (no queues)
  assertEquals(seen, [1, 2, -1]);
});

Deno.test("core-status: error folds every broken sidecar into subsystems", () => {
  const { svc, set } = withSidecars();
  svc.noteBootDone();
  set([
    { kind: "llama", status: "Error", message: "exited with code 1" },
    { kind: "speech", status: "Error", message: "model not found" },
    { kind: "llama-embed", status: "Running" },
  ]);
  const s = svc.snapshot();
  assertEquals(s.status, "error");
  assertEquals(s.subsystems, [
    { kind: "llama", status: "Error", message: "exited with code 1" },
    { kind: "speech", status: "Error", message: "model not found" },
    { kind: "llama-embed", status: "Running", message: undefined },
  ]);
  // The collapsed detail names the first broken subsystem.
  assertEquals(s.detail, "exited with code 1");
});

Deno.test("core-status: a sidecar loading drives starting_up with progress", () => {
  const { svc, set } = withSidecars();
  svc.noteBootDone();
  set([{ kind: "speech", status: "Loading", message: "loading", progress: 0.4 }]);
  const s = svc.snapshot();
  assertEquals(s.status, "starting_up");
  assertEquals(s.detail, "loading speech");
  assertEquals(s.progress, 0.4);
});

// A minimal download row; only status / sizeBytes / downloadedBytes matter to
// the aggregate fold.
function dl(
  status: DownloadStatus,
  sizeBytes: number | undefined,
  downloadedBytes = 0,
): DownloadEntry {
  return {
    id: `models:file-${status}-${sizeBytes}-${downloadedBytes}`,
    source: "@test/repo/main/file.gguf",
    destination: "models",
    relPath: "test/repo/file.gguf",
    absPath: "/models/test/repo/file.gguf",
    filename: "file.gguf",
    groupId: "llm",
    sizeBytes,
    downloadedBytes,
    status,
    addedAtMs: 0,
  };
}

Deno.test("core-status: active downloads drive downloading with aggregate progress", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteDownloads([dl("Downloading", 100, 50), dl("Pending", 100), dl("Completed", 100, 100)]);
  const s = svc.snapshot();
  assertEquals(s.status, "downloading");
  assertEquals(s.detail, "2 files");
  assertEquals(s.progress, 0.25);
  // Every row finished -> back to idle.
  svc.noteDownloads([dl("Completed", 100, 100)]);
  assertEquals(svc.snapshot().status, "idle");
});

Deno.test("core-status: busy outranks downloading", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteDownloads([dl("Downloading", 100, 10)]);
  assertEquals(svc.snapshot().status, "downloading");
  svc.noteActiveStreams(1);
  assertEquals(svc.snapshot().status, "busy");
  svc.noteActiveStreams(0);
  assertEquals(svc.snapshot().status, "downloading");
});

Deno.test("core-status: an unsized active download reports no progress", () => {
  const svc = fresh();
  svc.noteBootDone();
  svc.noteDownloads([dl("Downloading", undefined, 512)]);
  const s = svc.snapshot();
  assertEquals(s.status, "downloading");
  assertEquals(s.detail, "1 file");
  assertEquals(s.progress, undefined);
});

Deno.test("core-status: a message-only sidecar change re-emits", () => {
  const { svc, set } = withSidecars();
  svc.noteBootDone();
  const seen: (string | undefined)[] = [];
  svc.subscribe((s) => seen.push(s.subsystems[0]?.message));
  set([{ kind: "llama", status: "Error", message: "first error" }]);
  set([{ kind: "llama", status: "Error", message: "second error" }]);
  assertEquals(seen, ["first error", "second error"]);
});
