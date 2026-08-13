// Unit coverage for the settings -> required-files mapping (the single source
// of truth shared by core + client). Pure functions, no I/O.

import { assertEquals } from "@std/assert";
import { TRIPLES } from "./model.ts";
import { binaryUnavailableOnTriple, requiredBinaryKinds } from "./model.ts";
import { requiredModelRefs } from "./settings/engine.ts";

Deno.test("requiredModelRefs: llm local with image support yields model + mmproj", () => {
  const refs = requiredModelRefs({
    "llm.provider": "local",
    "llm.modelPath": "@u/r/main/m.gguf",
    "llm.supportImages": true,
    "llm.mmprojPath": "@u/r/main/mm.gguf",
  });
  assertEquals(
    refs.filter((r) => r.group === "llm").map((r) => r.source),
    ["@u/r/main/m.gguf", "@u/r/main/mm.gguf"],
  );
});

Deno.test("requiredModelRefs: external llm contributes no llm model", () => {
  const refs = requiredModelRefs({
    "llm.provider": "external",
    "llm.modelPath": "@u/r/main/m.gguf",
  });
  assertEquals(
    refs.some((r) => r.group === "llm"),
    false,
  );
});

Deno.test("requiredModelRefs: stt bundle files only when enabled + local", () => {
  const base = {
    "stt.modelFiles": JSON.stringify({
      encoder: "@u/r/main/enc.onnx",
      decoder: "@u/r/main/dec.onnx",
      tokens: "@u/r/main/tokens.txt",
    }),
  };
  assertEquals(
    requiredModelRefs({ ...base, "stt.enabled": false }).some((r) => r.group === "stt"),
    false,
  );
  assertEquals(
    requiredModelRefs({
      ...base,
      "stt.enabled": true,
      "stt.provider": "external",
    }).some((r) => r.group === "stt"),
    false,
  );
  assertEquals(
    requiredModelRefs({ ...base, "stt.enabled": true, "stt.provider": "local" })
      .filter((r) => r.group === "stt")
      .map((r) => r.source),
    ["@u/r/main/enc.onnx", "@u/r/main/dec.onnx", "@u/r/main/tokens.txt"],
  );
});

Deno.test("requiredModelRefs: tts bundle files only when tts.enabled; embed always", () => {
  const off = requiredModelRefs({});
  assertEquals(
    off.some((r) => r.group === "tts"),
    false,
  );
  assertEquals(
    off.some((r) => r.group === "embed"),
    true,
  );

  // tts.enabled without a bundle yields no tts refs (nothing to download yet).
  assertEquals(
    requiredModelRefs({ "tts.enabled": true }).some((r) => r.group === "tts"),
    false,
  );

  const on = requiredModelRefs({
    "tts.enabled": true,
    "tts.modelFiles": JSON.stringify({
      model: "@u/r/main/model.onnx",
      voices: "@u/r/main/voices.bin",
      tokens: "@u/r/main/tokens.txt",
    }),
  });
  assertEquals(
    on.filter((r) => r.group === "tts").map((r) => r.source),
    ["@u/r/main/model.onnx", "@u/r/main/voices.bin", "@u/r/main/tokens.txt"],
  );
});

Deno.test("requiredBinaryKinds: deno + llama-server always; speech gated on local STT or TTS", () => {
  // STT and TTS are off by default, so a fresh install (even with an external
  // LLM) needs only the always-on pair - voice is opt-in.
  assertEquals(requiredBinaryKinds({ "llm.provider": "external" }).sort(), [
    "deno",
    "llama-server",
  ]);
  // Both engines off: just the always-on pair.
  assertEquals(requiredBinaryKinds({ "stt.enabled": false, "tts.enabled": false }).sort(), [
    "deno",
    "llama-server",
  ]);
  // Local STT pulls in the combined speech binary.
  assertEquals(
    requiredBinaryKinds({
      "llm.provider": "local",
      "stt.enabled": true,
      "stt.provider": "local",
    }).sort(),
    ["deno", "llama-server", "loca-core-speech"],
  );
  // TTS alone also pulls it in (STT off).
  assertEquals(requiredBinaryKinds({ "stt.enabled": false, "tts.enabled": true }).sort(), [
    "deno",
    "llama-server",
    "loca-core-speech",
  ]);
  // External STT with TTS off needs no local speech binary.
  assertEquals(
    requiredBinaryKinds({
      "stt.enabled": true,
      "stt.provider": "external",
      "tts.enabled": false,
    }).sort(),
    ["deno", "llama-server"],
  );
});

Deno.test("binaryUnavailableOnTriple: resolver-backed binaries available on all triples", () => {
  for (const triple of TRIPLES) {
    assertEquals(binaryUnavailableOnTriple("llama-server", triple), false);
    assertEquals(binaryUnavailableOnTriple("deno", triple), false);
  }
});

Deno.test("binaryUnavailableOnTriple: self-hosted speech available on all triples", () => {
  for (const triple of TRIPLES) {
    assertEquals(binaryUnavailableOnTriple("loca-core-speech", triple), false);
  }
});
