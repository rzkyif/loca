// Handle-level live E2E of the runtime permission prompt flow on WINDOWS: a
// REAL WorkerHandle spawns the REAL loca-core-ptyhost + deno + tool-worker
// chain in socket mode (ConPTY + loopback control socket) and drives one tool
// call whose accesses trigger real Deno prompts. This is the Windows
// counterpart to the unix-only prompt-live-probe.test.ts: it is the only cover
// that exercises the ConPTY + control-socket composition end to end, and the
// ConPTY exists only on Windows. Asserts the full loop: boot, forwarded prompt
// frame, user answer, resumed access, auto-denied undeclared access, exit
// propagation through terminate().
//
// Windows-only. The unix PTY path (protocol on stdout, echo cancellation) is
// covered by prompt-live-probe.test.ts, so this test deliberately does not run
// there: driving the same flow through the heavier WorkerHandle harness adds no
// coverage on unix and is sensitive to PTY answer/resume timing. The
// platform-independent socket transport itself is unit-covered on every OS by
// control-socket.test.ts.
//
// Skipped when the ptyhost binary is absent (no cargo build yet). Note that the
// CI test:js lane runs on Linux only, so this test does not currently execute
// in CI; it is the local Windows validator for the ConPTY prompt path.

import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { WorkerHandle } from "./worker-handle.ts";
import type { WorkerToPoolFrame } from "./worker-protocol.ts";

function findPtyhost(): string | null {
  const repoRoot = fromFileUrl(new URL("../../../../", import.meta.url));
  const exe = Deno.build.os === "windows" ? ".exe" : "";
  for (const candidate of [
    `${repoRoot}target/debug/loca-core-ptyhost${exe}`,
    `${repoRoot}target/release/loca-core-ptyhost${exe}`,
  ]) {
    try {
      if (Deno.statSync(candidate).isFile) return candidate;
    } catch {
      /* try next */
    }
  }
  return null;
}

const ptyhost = findPtyhost();

// The tool reads a declared-but-ask path (prompts; the test answers allow via
// the forwarded frame) and writes an undeclared path (prompts; auto-denied by
// the extension's `deny` policy without user involvement).
const ENTRY = `
export async function probe(args) {
  const granted = await Deno.readTextFile(args.grantedPath);
  let deniedName = null;
  try {
    await Deno.writeTextFile(args.deniedPath, "x");
  } catch (e) {
    deniedName = e.name;
  }
  return { grantedLen: granted.length, deniedName };
}
`;

// Plant a binary into the fake core root's bin dir. Hardlink when possible
// (deno is >100 MB), copy when the tempdir is on another filesystem.
async function plant(src: string, dest: string): Promise<void> {
  try {
    await Deno.link(src, dest);
  } catch {
    await Deno.copyFile(src, dest);
  }
}

Deno.test({
  name: "worker-handle live (windows conpty): prompt forwards, user allow resumes, undeclared write auto-denies",
  ignore: ptyhost === null || Deno.build.os !== "windows",
  // The subprocess tree (ptyhost -> deno) and its pumps are torn down by
  // terminate(); sanitizers misread the piped readers that race it.
  sanitizeResources: false,
  sanitizeOps: false,
  async fn() {
    const root = await Deno.makeTempDir({ prefix: "loca-whl-" });
    const savedEnv: Record<string, string | undefined> = {};
    for (const key of ["LOCA_CORE_HOME", "LOCA_CHANNEL"]) {
      savedEnv[key] = Deno.env.get(key);
    }
    Deno.env.set("LOCA_CORE_HOME", root);
    Deno.env.set("LOCA_CHANNEL", "stable");
    let handle: WorkerHandle | undefined;
    try {
      const exe = Deno.build.os === "windows" ? ".exe" : "";
      await Deno.mkdir(join(root, "bin"), { recursive: true });
      await Deno.mkdir(join(root, "workers"), { recursive: true });
      await plant(ptyhost as string, join(root, "bin", `loca-core-ptyhost${exe}`));
      await plant(Deno.execPath(), join(root, "bin", `deno${exe}`));
      await Deno.copyFile(
        fromFileUrl(new URL("../workers/tool-worker.ts", import.meta.url)),
        join(root, "workers", "tool-worker.ts"),
      );

      const extensionFolder = join(root, "test-extension");
      await Deno.mkdir(extensionFolder, { recursive: true });
      const entryPath = join(extensionFolder, "entry.ts");
      await Deno.writeTextFile(entryPath, ENTRY);

      // Outside every allow-read'd path, so the read genuinely prompts. Windows
      // tempdirs are not symlinked, so makeTempFile's path equals the prompt's
      // resource string as-is (no realPath normalization needed).
      const grantedPath = await Deno.makeTempFile({ suffix: ".txt" });
      await Deno.writeTextFile(grantedPath, "twelve chars");
      const deniedPath = join(await Deno.makeTempDir(), "denied.txt");

      handle = WorkerHandle.spawn({
        extensionId: "live-ext",
        entryPath,
        extensionFolder,
        flags: [], // nothing granted: both accesses go through the prompt path
        promptContext: {
          required: [{ kind: "read", path: grantedPath, reason: "live test" }],
          grants: [], // no grant row -> ask -> forward to the user
          undeclaredPolicy: "deny",
          templates: {
            home: "",
            downloads: "",
            models: "",
            sessions: "",
            extension: extensionFolder,
          },
        },
      });

      const frames: WorkerToPoolFrame[] = [];
      const waiters: Array<{
        match: (f: WorkerToPoolFrame) => boolean;
        resolve: (f: WorkerToPoolFrame) => void;
      }> = [];
      handle.on((frame) => {
        frames.push(frame);
        for (let i = waiters.length - 1; i >= 0; i--) {
          if (waiters[i].match(frame)) {
            const [w] = waiters.splice(i, 1);
            w.resolve(frame);
          }
        }
      });
      const nextFrame = (
        label: string,
        match: (f: WorkerToPoolFrame) => boolean,
        timeoutMs = 30_000,
      ): Promise<WorkerToPoolFrame> => {
        const hit = frames.find(match);
        if (hit) return Promise.resolve(hit);
        return new Promise((resolve, reject) => {
          const t = setTimeout(
            () =>
              reject(new Error(`timed out waiting for ${label}; saw ${JSON.stringify(frames)}`)),
            timeoutMs,
          );
          waiters.push({
            match,
            resolve: (f) => {
              clearTimeout(t);
              resolve(f);
            },
          });
        });
      };

      await handle.waitForBoot();
      handle.send({
        kind: "call",
        callId: "c1",
        toolName: "probe",
        fnExport: "probe",
        arguments: JSON.stringify({ grantedPath, deniedPath }),
        chatContext: { userMessage: "hi", sessionId: null },
      });
      handle.inFlightCalls = 1; // single-call attribution, as the pool tracks it

      // The declared-but-ungranted read forwards to the user with its reason.
      const prompt = await nextFrame("permission_prompt", (f) => f.kind === "permission_prompt");
      assert(prompt.kind === "permission_prompt");
      assertEquals(prompt.permission, "read");
      assertEquals(prompt.resource, grantedPath);
      assertEquals(prompt.declared, true);
      assertEquals(prompt.reason, "live test");

      handle.answerPrompt(prompt.requestId, true);

      // The read resumes with data; the undeclared write is auto-denied and
      // surfaces to the tool as NotCapable, without a second forwarded prompt.
      const result = await nextFrame("tool_result", (f) => f.kind === "tool_result");
      assert(result.kind === "tool_result");
      assertEquals(result.callId, "c1");
      assertEquals(result.result, { grantedLen: 12, deniedName: "NotCapable" });
      assertEquals(
        frames.filter((f) => f.kind === "permission_prompt").length,
        1,
        "undeclared write must be auto-denied, not forwarded",
      );
      assert(
        frames.some((f) => f.kind === "stderr_log" && f.line.includes("auto-denied write")),
        `expected an auto-denied write log; saw ${JSON.stringify(frames)}`,
      );
      assertEquals(handle.promptAnsweredByUser, true, "worker must be flagged for retirement");

      // Exit propagation: shutdown must reach the worker and the ptyhost must
      // mirror its exit, or terminate() would hang until its kill fallbacks.
      const t0 = Date.now();
      await handle.terminate();
      handle = undefined;
      assert(Date.now() - t0 < 2_000, "worker should exit on shutdown, not via kill fallback");
    } finally {
      await handle?.terminate();
      for (const [key, value] of Object.entries(savedEnv)) {
        if (value === undefined) Deno.env.delete(key);
        else Deno.env.set(key, value);
      }
      await Deno.remove(root, { recursive: true }).catch(() => {});
    }
  },
});
