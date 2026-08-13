// The pure transcribe -> autocorrect -> merge chain. Stub deps stand in for the
// core API so we can assert the decision logic: error/empty short-circuits, the
// autocorrect-changed -> `original` rule, and that merge only runs when there is
// prior text to chain onto. The default core-bound deps are covered by the app.

import { describe, expect, it, vi } from "vitest";
import { runTranscriptionChain, type TranscriptionDeps } from "./transcription";

const deps = (over: Partial<TranscriptionDeps> = {}): TranscriptionDeps => ({
  transcribe: () => Promise.resolve({ text: "hello world" }),
  autocorrect: (t) => Promise.resolve(t),
  merge: (_prior, next) => Promise.resolve(next),
  ...over,
});

const opts = { autocorrect: false, chain: false };

describe("runTranscriptionChain", () => {
  it("returns error when transcribe reports one", async () => {
    const out = await runTranscriptionChain(
      "b64",
      "",
      opts,
      deps({ transcribe: () => Promise.resolve({ text: "", error: "boom" }) }),
    );
    expect(out).toEqual({ kind: "error", message: "boom" });
  });

  it("returns empty when the transcription is blank", async () => {
    const out = await runTranscriptionChain(
      "b64",
      "",
      opts,
      deps({ transcribe: () => Promise.resolve({ text: "   " }) }),
    );
    expect(out).toEqual({ kind: "empty" });
  });

  it("passes the trimmed raw text through when no post-processing is on", async () => {
    const out = await runTranscriptionChain(
      "b64",
      "",
      opts,
      deps({ transcribe: () => Promise.resolve({ text: "  hi there  " }) }),
    );
    expect(out).toEqual({ kind: "ok", text: "hi there", original: null });
  });

  it("sets `original` only when autocorrect changes the text", async () => {
    const changed = await runTranscriptionChain(
      "b64",
      "",
      { autocorrect: true, chain: false },
      deps({
        transcribe: () => Promise.resolve({ text: "helo" }),
        autocorrect: () => Promise.resolve("hello"),
      }),
    );
    expect(changed).toEqual({ kind: "ok", text: "hello", original: "helo" });

    const unchanged = await runTranscriptionChain(
      "b64",
      "",
      { autocorrect: true, chain: false },
      deps({
        transcribe: () => Promise.resolve({ text: "hello" }),
        autocorrect: () => Promise.resolve("hello"),
      }),
    );
    expect(unchanged).toEqual({ kind: "ok", text: "hello", original: null });
  });

  it("merges onto existing text only when there is prior text", async () => {
    const merge = vi.fn((prior: string, next: string) => Promise.resolve(`${prior} ${next}`));
    const withPrior = await runTranscriptionChain(
      "b64",
      "before",
      { autocorrect: false, chain: true },
      deps({ transcribe: () => Promise.resolve({ text: "after" }), merge }),
    );
    expect(withPrior).toEqual({
      kind: "ok",
      text: "before after",
      original: null,
    });
    expect(merge).toHaveBeenCalledTimes(1);

    merge.mockClear();
    const noPrior = await runTranscriptionChain(
      "b64",
      "   ",
      { autocorrect: false, chain: true },
      deps({ transcribe: () => Promise.resolve({ text: "after" }), merge }),
    );
    expect(noPrior).toEqual({ kind: "ok", text: "after", original: null });
    expect(merge).not.toHaveBeenCalled();
  });

  it("merges both raw and corrected variants when chaining after autocorrect", async () => {
    const out = await runTranscriptionChain(
      "b64",
      "prior",
      { autocorrect: true, chain: true },
      deps({
        transcribe: () => Promise.resolve({ text: "helo" }),
        autocorrect: () => Promise.resolve("hello"),
        merge: (prior, next) => Promise.resolve(`${prior}+${next}`),
      }),
    );
    // raw -> "prior+helo", corrected -> "prior+hello"; they differ, so original set.
    expect(out).toEqual({
      kind: "ok",
      text: "prior+hello",
      original: "prior+helo",
    });
  });

  it("falls back to raw text and warns when autocorrect throws", async () => {
    const onWarn = vi.fn();
    const out = await runTranscriptionChain(
      "b64",
      "",
      { autocorrect: true, chain: false },
      deps({
        transcribe: () => Promise.resolve({ text: "hi" }),
        autocorrect: () => Promise.reject(new Error("nope")),
      }),
      onWarn,
    );
    expect(out).toEqual({ kind: "ok", text: "hi", original: null });
    expect(onWarn).toHaveBeenCalledWith("autocorrect", expect.any(Error));
  });
});
