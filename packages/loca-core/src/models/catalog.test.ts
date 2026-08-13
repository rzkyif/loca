// Catalog sha256 pinning, exercised against the real authored catalog
// (network-free, like stt.test.ts). Guards that every downloadable file carries
// a pinned content hash so a model download verifies against the SIGNED catalog
// instead of trusting HF's x-linked-etag, and that the index the download path
// consults resolves those hashes.

import { assert, assertEquals, assertMatch } from "@std/assert";
import { catalogModels } from "@loca/shared";
import { buildCatalogPayload } from "../../../loca-model-catalog/src/index.ts";
import { buildSha256Index } from "./catalog.ts";

const catalog = buildCatalogPayload("2026-06-19T00:00:00Z");
const SHA256 = /^[0-9a-f]{64}$/;

Deno.test("catalog: every GGUF quant + mmproj carries a pinned sha256", () => {
  for (const model of catalogModels(catalog)) {
    for (const v of model.variants) {
      for (const q of v.quants) {
        assert(q.sha256 !== undefined, `missing sha256 for ${q.modelSpec}`);
        assertMatch(q.sha256, SHA256);
      }
      if (v.mmprojSpec) {
        assert(v.mmprojSha256 !== undefined, `missing mmproj sha256 for ${v.mmprojSpec}`);
        assertMatch(v.mmprojSha256, SHA256);
      }
    }
  }
});

Deno.test("catalog: every STT/TTS bundle file carries a pinned sha256", () => {
  for (const cat of [catalog.stt, catalog.tts]) {
    for (const model of cat.models) {
      for (const quant of model.quants) {
        for (const f of quant.files) {
          assert(f.sha256 !== undefined, `missing sha256 for ${f.modelSpec}`);
          assertMatch(f.sha256, SHA256);
        }
      }
    }
  }
});

Deno.test("buildSha256Index: covers every distinct downloadable spec", () => {
  const idx = buildSha256Index(catalog);
  // Every distinct GGUF quant, mmproj, and speech-file spec is indexed (a spec
  // reused across variants is one entry).
  const specs = new Set<string>();
  for (const model of catalogModels(catalog)) {
    for (const v of model.variants) {
      if (v.mmprojSpec) specs.add(v.mmprojSpec);
      for (const q of v.quants) specs.add(q.modelSpec);
    }
  }
  for (const cat of [catalog.stt, catalog.tts]) {
    for (const model of cat.models) {
      for (const q of model.quants) {
        for (const f of q.files) specs.add(f.modelSpec);
      }
    }
  }
  assertEquals(idx.size, specs.size);
  for (const spec of specs) assertMatch(idx.get(spec)!, SHA256);

  // A spec absent from the catalog (a custom user pick) is not in the index, so
  // the download path falls back to HF's published hash for it.
  assertEquals(idx.get("@someone/custom-repo/main/model.gguf"), undefined);

  // A real spec resolves to its quant's pinned sha256.
  const sample = catalogModels(catalog)[0].variants[0].quants[0];
  assertEquals(idx.get(sample.modelSpec), sample.sha256);
});
