// sttUsesLocal / ttsUsesLocal: the single predicate every local-vs-external
// speech gate (model + binary requirements, the speech sidecar's desired state)
// shares. The key property is that a SPARSE settings object (the on-disk file
// stores only non-default values) yields the same answer as a resolved one, so
// the gates cannot drift on an absent flag or an absent provider.

import { assertEquals } from "@std/assert";
import {
  assetVariants,
  binaryUnavailableOnTriple,
  platformVariants,
  requiredBinaryKinds,
  sttUsesLocal,
  ttsUsesLocal,
} from "./model.ts";

Deno.test("sttUsesLocal: STT is disabled by default, so an empty (sparse) settings means not local", () => {
  assertEquals(sttUsesLocal({}), false);
});

Deno.test("sttUsesLocal: an explicit external provider opts out even when enabled", () => {
  assertEquals(sttUsesLocal({ "stt.enabled": true, "stt.provider": "external" }), false);
});

Deno.test("sttUsesLocal: enabling STT without naming a provider still resolves to local", () => {
  // A user who flips STT on but never touches the provider persists only
  // { stt.enabled: true }; the absent provider must read as local, not off.
  assertEquals(sttUsesLocal({ "stt.enabled": true }), true);
});

Deno.test("sttUsesLocal: an explicit disable also resolves to not local", () => {
  assertEquals(sttUsesLocal({ "stt.enabled": false }), false);
});

Deno.test("ttsUsesLocal: TTS is disabled by default, so an empty settings means not local", () => {
  assertEquals(ttsUsesLocal({}), false);
});

Deno.test("ttsUsesLocal: enabling TTS without naming a provider resolves to local", () => {
  assertEquals(ttsUsesLocal({ "tts.enabled": true }), true);
});

Deno.test("ttsUsesLocal: an explicit external provider stays external", () => {
  assertEquals(ttsUsesLocal({ "tts.enabled": true, "tts.provider": "external" }), false);
});

Deno.test("requiredBinaryKinds: the speech binary is required when either engine is local", () => {
  // Default settings: STT off, TTS off -> no speech binary (voice is opt-in).
  assertEquals(requiredBinaryKinds({}).includes("loca-core-speech"), false);
  // Enabling STT locally pulls in the speech binary.
  assertEquals(requiredBinaryKinds({ "stt.enabled": true }).includes("loca-core-speech"), true);
  // Enabled but external -> still no speech binary.
  assertEquals(
    requiredBinaryKinds({
      "stt.enabled": true,
      "stt.provider": "external",
      "tts.enabled": false,
    }).includes("loca-core-speech"),
    false,
  );
});

Deno.test("assetVariants: a bare string normalizes to the cpu variant", () => {
  assertEquals(assetVariants("llama-{tag}-bin-macos-arm64.tar.gz"), {
    cpu: { asset: "llama-{tag}-bin-macos-arm64.tar.gz" },
  });
  assertEquals(assetVariants(undefined), {});
});

Deno.test("assetVariants: a per-variant map keeps each variant, normalizing bare strings", () => {
  assertEquals(
    assetVariants({
      cpu: "cpu.zip",
      cuda: { asset: "cuda.zip", extra: ["cudart.zip"] },
    }),
    {
      cpu: { asset: "cpu.zip" },
      cuda: { asset: "cuda.zip", extra: ["cudart.zip"] },
    },
  );
});

Deno.test("platformVariants: a bare pinned target is the cpu variant", () => {
  assertEquals(platformVariants({ url: "u", sha256: "s" }), { cpu: { url: "u", sha256: "s" } });
  assertEquals(platformVariants({ cuda: { url: "u", sha256: "s" } }), {
    cuda: { url: "u", sha256: "s" },
  });
  assertEquals(platformVariants(undefined), {});
});

Deno.test("binaryUnavailableOnTriple: llama-server is available on every shipped triple", () => {
  // Every triple keeps a cpu variant, so none is unavailable.
  for (const t of [
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
  ] as const) {
    assertEquals(binaryUnavailableOnTriple("llama-server", t), false);
  }
});
