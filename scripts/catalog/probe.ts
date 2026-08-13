// Dev tool: probe Unsloth GGUF repos for the data needed to author a
// `@loca/model-catalog` family file. For each repo it lists the quant files +
// sizes + mmproj, and range-reads one GGUF header to extract the architecture
// fields (block/embedding/head counts, head_dim, context length) used by the
// on-device fit engine's KV-cache math.
//
// Usage: deno run -A scripts/catalog/probe.ts unsloth/Qwen3.5-2B-GGUF [...repos]

const HF = "https://huggingface.co";

type Arch = {
  architecture: string;
  blockCount: number;
  embeddingLength: number;
  headCount: number;
  headCountKv: number;
  keyLength?: number;
  contextLength: number;
};

const GGUF_MAGIC = 0x46554747; // "GGUF" little-endian

// GGUF value type ids.
const T = {
  UINT8: 0,
  INT8: 1,
  UINT16: 2,
  INT16: 3,
  UINT32: 4,
  INT32: 5,
  FLOAT32: 6,
  BOOL: 7,
  STRING: 8,
  ARRAY: 9,
  UINT64: 10,
  INT64: 11,
  FLOAT64: 12,
} as const;

class Reader {
  off = 0;
  constructor(private buf: DataView) {}
  u32() {
    const v = this.buf.getUint32(this.off, true);
    this.off += 4;
    return v;
  }
  u64() {
    const v = Number(this.buf.getBigUint64(this.off, true));
    this.off += 8;
    return v;
  }
  scalarSize(t: number): number {
    switch (t) {
      case T.UINT8:
      case T.INT8:
      case T.BOOL:
        return 1;
      case T.UINT16:
      case T.INT16:
        return 2;
      case T.UINT32:
      case T.INT32:
      case T.FLOAT32:
        return 4;
      case T.UINT64:
      case T.INT64:
      case T.FLOAT64:
        return 8;
      default:
        throw new Error(`non-scalar type ${t}`);
    }
  }
  str(): string {
    const len = this.u64();
    const bytes = new Uint8Array(this.buf.buffer, this.buf.byteOffset + this.off, len);
    this.off += len;
    return new TextDecoder().decode(bytes);
  }
  // Read a metadata value of the given type, returning a number for scalars and
  // the typed value otherwise. Crucially advances `off` past arrays so parsing
  // can continue regardless of value type.
  value(t: number): number | string | number[] | null {
    if (t === T.STRING) return this.str();
    if (t === T.ARRAY) {
      const elemType = this.u32();
      const count = this.u64();
      if (elemType === T.STRING) {
        for (let i = 0; i < count; i++) this.str();
        return null;
      }
      // Keep small numeric arrays (per-layer configs like head_count_kv); skip
      // huge ones (token-type ids) to bound memory.
      if (count <= 4096) {
        const out: number[] = [];
        for (let i = 0; i < count; i++) {
          out.push(this.value(elemType) as number);
        }
        return out;
      }
      this.off += this.scalarSize(elemType) * count;
      return null;
    }
    if (t === T.FLOAT32) {
      const v = this.buf.getFloat32(this.off, true);
      this.off += 4;
      return v;
    }
    if (t === T.FLOAT64) {
      const v = this.buf.getFloat64(this.off, true);
      this.off += 8;
      return v;
    }
    if (t === T.UINT64 || t === T.INT64) return this.u64();
    if (t === T.BOOL || t === T.UINT8 || t === T.INT8) {
      const v = this.buf.getUint8(this.off);
      this.off += 1;
      return v;
    }
    if (t === T.UINT16 || t === T.INT16) {
      const v = this.buf.getUint16(this.off, true);
      this.off += 2;
      return v;
    }
    return this.u32(); // UINT32/INT32
  }
}

async function fetchRange(url: string, bytes: number): Promise<DataView> {
  const res = await fetch(url, { headers: { Range: `bytes=0-${bytes - 1}` } });
  if (!res.ok && res.status !== 206) {
    throw new Error(`HTTP ${res.status} for ${url}`);
  }
  const buf = await res.arrayBuffer();
  return new DataView(buf);
}

async function readArch(ggufUrl: string): Promise<Arch> {
  // 48 MiB comfortably covers the metadata (incl. tokenizer arrays) for these
  // models; bump if a future model's vocab is larger.
  const dv = await fetchRange(ggufUrl, 48 * 1024 * 1024);
  const r = new Reader(dv);
  if (r.u32() !== GGUF_MAGIC) throw new Error("not a GGUF file");
  r.u32(); // version
  r.u64(); // tensor_count
  const kvCount = r.u64();
  const kv: Record<string, number | string | null> = {};
  for (let i = 0; i < kvCount; i++) {
    const key = r.str();
    const type = r.u32();
    kv[key] = r.value(type);
  }
  const arch = String(kv["general.architecture"]);
  // Some models (e.g. gemma-4-12b) store head_count / head_count_kv as a
  // per-layer array; collapse to the max (constant across layers in practice).
  const num = (suffix: string): number => {
    const v = kv[`${arch}.${suffix}`];
    return Array.isArray(v) ? Math.max(...v) : (v as number);
  };
  return {
    architecture: arch,
    blockCount: num("block_count"),
    embeddingLength: num("embedding_length"),
    headCount: num("attention.head_count"),
    headCountKv: num("attention.head_count_kv"),
    keyLength: num("attention.key_length") || undefined,
    contextLength: num("context_length"),
  };
}

async function tree(repo: string): Promise<Array<{ path: string; size: number }>> {
  const res = await fetch(`${HF}/api/models/${repo}/tree/main?recursive=true`);
  const json = await res.json();
  return json
    .filter((e: { path: string }) => e.path.endsWith(".gguf"))
    .map((e: { path: string; size: number; lfs?: { size: number } }) => ({
      path: e.path,
      size: e.lfs?.size ?? e.size,
    }));
}

// Split GGUFs are sharded as "name-00001-of-00003.gguf". Collapse a shard set
// into one quant: the 00001 path (llama.cpp auto-loads the rest) and the summed
// size. Single-file quants pass through unchanged.
function collapseShards(
  files: Array<{ path: string; size: number }>,
): Array<{ path: string; size: number }> {
  const groups = new Map<string, { path: string; size: number }>();
  for (const f of files) {
    const m = f.path.match(/^(.*)-(\d{5})-of-(\d{5})\.gguf$/);
    const key = m ? `${m[1]}.gguf` : f.path;
    const isFirst = m ? m[2] === "00001" : true;
    const cur = groups.get(key);
    if (!cur) groups.set(key, { path: isFirst ? f.path : "", size: f.size });
    else {
      cur.size += f.size;
      if (isFirst) cur.path = f.path;
    }
  }
  return [...groups.values()].filter((g) => g.path);
}

for (const repo of Deno.args) {
  const all = await tree(repo);
  // Keep top-level files only: drop speculative-decoding draft/assistant/MTP
  // models, which some repos bundle in subfolders (e.g. "MTP/...").
  const files = all.filter((f) => !f.path.includes("/") && !/draft|assistant|mtp/i.test(f.path));
  const quants = collapseShards(files.filter((f) => !/mmproj/i.test(f.path)));
  const mmproj =
    files.find((f) => /mmproj-F16/i.test(f.path)) ?? files.find((f) => /mmproj/i.test(f.path));
  // Probe the smallest quant header (arch is identical across quants).
  const smallest = [...quants].sort((a, b) => a.size - b.size)[0];
  const arch = await readArch(`${HF}/${repo}/resolve/main/${smallest.path}`);
  console.log(JSON.stringify({ repo, arch, mmproj, quants }, null, 2));
}
