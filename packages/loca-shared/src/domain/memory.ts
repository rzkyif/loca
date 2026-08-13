// Memory shapes shared between core and client. A memory is either a piece of
// reference *knowledge* (a markdown file) or a *skill* (a folder with a
// SKILL.md instruction body plus optional bundled reference files). Metadata
// plus the background-generated summary live in SQLite; content is loaded on
// get.

export type MemoryKind = "knowledge" | "skill";

// Marker provider for user-authored, editable memories. Any other value is the
// id of the extension that ships the memory: such memories are read-only and
// refreshed only by reinstalling their extension.
export const USER_MEMORY_PROVIDER = "user";

export interface MemoryMeta {
  id: string;
  kind: MemoryKind;
  title: string;
  // Path identifying the memory under the core's memories directory. Knowledge
  // is a file (`<slug>.md`); a skill is a folder (`<slug>`) holding SKILL.md.
  filename: string;
  contentHash: string;
  // LLM-generated; absent until the background indexer has run.
  summary?: string;
  // Whether the summary (and embedding) no longer match the current content:
  // true when the memory has never been indexed or was edited since. While
  // stale the memory does not surface in auto-relevance, so the UI flags it.
  summaryStale?: boolean;
  // `USER_MEMORY_PROVIDER` for editable user memories, otherwise the shipping
  // extension id (read-only).
  provider: string;
  // Whether the memory participates in chat (auto-relevance + @/#// triggers).
  enabled: boolean;
  // Skills only: advisory tool names the skill tends to use, and the names of
  // the bundled reference files alongside SKILL.md (loaded on demand).
  suggestedTools?: string[];
  files?: string[];
  createdAtMs: number;
  updatedAtMs: number;
}

export interface Memory extends MemoryMeta {
  content: string;
}
