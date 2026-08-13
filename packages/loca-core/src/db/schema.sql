-- loca-core SQLite schema. Idempotent: every CREATE uses IF NOT EXISTS, so
-- running this on boot is safe regardless of prior state.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Paired clients
CREATE TABLE IF NOT EXISTS clients (
  id              TEXT PRIMARY KEY,           -- ULID
  name            TEXT NOT NULL,
  token_hash      TEXT NOT NULL UNIQUE,       -- sha256(bearer token)
  created_at_ms   INTEGER NOT NULL,
  last_seen_ms    INTEGER NOT NULL,
  revoked         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS clients_token ON clients(token_hash);

-- Short-lived pairing codes. The code is stored in the clear (not hashed): the
-- pairing PAKE keys off the code value itself, so core needs it for the ≤10-min
-- code lifetime. Only one row is ever unclaimed at a time (mintPairingCode wipes
-- prior unclaimed rows), so lookups select the single claimed=0 row.
CREATE TABLE IF NOT EXISTS pairing_codes (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  code            TEXT NOT NULL,              -- plaintext 6-digit pairing code
  created_at_ms   INTEGER NOT NULL,
  expires_at_ms   INTEGER NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 0,
  claimed         INTEGER NOT NULL DEFAULT 0
);

-- Sessions, messages, and attachments are NOT in SQLite: they live as plain
-- JSON files under ~/.loca/<channel>/core/sessions/<id>/session.json, managed
-- by services/sessions-store.ts (one session.json per session, attachment bytes
-- alongside under attachments/).

-- Extensions (one row per npm package, local folder, or the built-in). The
-- `status` column tracks the gated lifecycle: 'downloaded' (files on disk, deps
-- not installed, content_hash not yet pinned), 'installed' (deps installed,
-- content_hash pinned, tools enable-able), 'drift' (on-disk content no longer
-- matches the pinned hash; all tools auto-disabled until the user re-confirms).
CREATE TABLE IF NOT EXISTS extensions (
  id              TEXT PRIMARY KEY,           -- flat folder name
  source          TEXT NOT NULL,              -- 'npm' | 'local' | 'seeded'
  display_name    TEXT NOT NULL,
  description     TEXT,
  version         TEXT NOT NULL,
  installed_path  TEXT NOT NULL,
  manifest_hash TEXT NOT NULL,
  content_hash    TEXT NOT NULL DEFAULT '',   -- '' until pinned at install; the trust anchor
  status          TEXT NOT NULL DEFAULT 'downloaded',  -- 'downloaded' | 'installed' | 'drift'
  has_deps        INTEGER NOT NULL DEFAULT 0,  -- 1 when deno.json/package.json declares deps
  has_database    INTEGER NOT NULL DEFAULT 0,  -- 1 when loca.json declares "database": true
  undeclared_policy TEXT NOT NULL DEFAULT 'deny',  -- 'deny' | 'ask': runtime prompts outside declared perms
  installed_at_ms INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL
);

-- Tools (per extension)
CREATE TABLE IF NOT EXISTS tools (
  id                       TEXT PRIMARY KEY,          -- ${extension_id}::${name}
  extension_id               TEXT NOT NULL REFERENCES extensions(id) ON DELETE CASCADE,
  name                     TEXT NOT NULL,
  description              TEXT NOT NULL,
  parameters_json          TEXT NOT NULL,
  triggers_json            TEXT NOT NULL,
  fn_export                TEXT NOT NULL,
  always_available         INTEGER NOT NULL DEFAULT 0,
  platforms_json           TEXT NOT NULL DEFAULT '[]',  -- OS gating; [] = every platform
  enabled                  INTEGER NOT NULL DEFAULT 0,
  required_permissions_json TEXT NOT NULL DEFAULT '[]',  -- flattened PermissionDecl[]
  UNIQUE(extension_id, name)
);

-- Per-tool embedding (for phase-1 relevance)
CREATE TABLE IF NOT EXISTS tool_embeddings (
  tool_id     TEXT PRIMARY KEY REFERENCES tools(id) ON DELETE CASCADE,
  dim         INTEGER NOT NULL,
  vector      BLOB NOT NULL,
  source_hash TEXT NOT NULL
);

-- Per-tool granular permission decisions. 'granted' (Always Allow) is baked
-- into the worker's --allow-* spawn flags; 'ask' (also the behavior when no
-- row exists) leaves the permission out so Deno prompts at the moment of
-- access; 'denied' auto-rejects the prompt. A tool is LLM-exposed iff no
-- non-optional declared permission has state='denied'.
CREATE TABLE IF NOT EXISTS grants (
  tool_id         TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  permission_key  TEXT NOT NULL,              -- e.g. 'net:api.example.com:443'
  permission_kind TEXT NOT NULL,              -- 'net'|'read'|'write'|'run'|'env'|'ffi'|'sys'|'memories'|'llm'|'tts'|'stt'
  state           TEXT NOT NULL DEFAULT 'ask',  -- granted|ask|denied
  granted_at_ms   INTEGER NOT NULL,
  PRIMARY KEY (tool_id, permission_key)
);

-- Memories: agent-readable reference knowledge and procedural skills. Their
-- on-disk form under core/memories/ is the source of truth (content_hash
-- detects drift on rescan): knowledge is a `<slug>.md` file, a skill is a
-- `<slug>/` folder holding SKILL.md plus optional bundled files. This table
-- carries the metadata plus the background-generated summary + embedding, each
-- pinned to the content hash it was derived from so the indexer can tell when
-- they are stale. `provider` is 'user' for editable memories or the id of the
-- extension that ships a read-only one; `suggested_tools` is a JSON array of
-- advisory tool names (skills only).
CREATE TABLE IF NOT EXISTS memories (
  id                    TEXT PRIMARY KEY,
  kind                  TEXT NOT NULL DEFAULT 'knowledge',
  title                 TEXT NOT NULL UNIQUE,
  filename              TEXT NOT NULL UNIQUE,   -- relative to core/memories/
  content_hash          TEXT NOT NULL,
  summary               TEXT,
  summary_source_hash   TEXT,
  embedding             BLOB,
  embedding_dim         INTEGER,
  embedding_source_hash TEXT,
  provider              TEXT NOT NULL DEFAULT 'user',
  enabled               INTEGER NOT NULL DEFAULT 1,
  suggested_tools       TEXT,
  created_at_ms         INTEGER NOT NULL,
  updated_at_ms         INTEGER NOT NULL
);

-- MCP servers: external Model Context Protocol servers the user configures as
-- another provider of tools, prompts, and resources. `kind` is 'stdio' (a
-- spawned subprocess: `command` + JSON `args_json`) or 'remote' (a streamable
-- HTTP/SSE endpoint at `url`). For stdio, `runtime` is 'custom' (run `command`
-- verbatim) or 'deno' (run `command` through the bundled deno binary, with
-- `deno_allow_all` / `deno_permissions_json` controlling its permissions).
-- Their tools/prompts/resources are fetched live on connect and cached in
-- memory, not persisted here. `tool_enabled_json` / `prompt_enabled_json` are
-- JSON arrays of the tool/prompt names the user enabled, so autocomplete and
-- tool selection aren't spammed by everything a server offers.
CREATE TABLE IF NOT EXISTS mcp_servers (
  id                  TEXT PRIMARY KEY,
  name                TEXT NOT NULL,
  kind                TEXT NOT NULL,              -- 'stdio' | 'remote'
  command             TEXT,                        -- stdio: executable or deno run target
  args_json           TEXT NOT NULL DEFAULT '[]',  -- stdio: argv
  runtime             TEXT NOT NULL DEFAULT 'custom', -- stdio: 'custom' | 'deno'
  deno_allow_all      INTEGER NOT NULL DEFAULT 1,  -- deno runtime: run with --allow-all
  deno_permissions_json TEXT NOT NULL DEFAULT '[]', -- deno runtime: manual permission flags
  url                 TEXT,                        -- remote: endpoint
  remote_auth         TEXT NOT NULL DEFAULT 'none', -- remote: 'none' | 'bearer' | 'oauth'
  enabled             INTEGER NOT NULL DEFAULT 0,
  has_auth            INTEGER NOT NULL DEFAULT 0,  -- remote 'bearer': a static token is stored in the vault
  oauth_authorized    INTEGER NOT NULL DEFAULT 0,  -- remote 'oauth': the auth-code flow completed, tokens in vault
  tool_enabled_json   TEXT NOT NULL DEFAULT '[]',
  prompt_enabled_json TEXT NOT NULL DEFAULT '[]',
  -- Tool names the user turned OFF "always available" for (MCP tools default to
  -- always-available so they're usually offered; a name here folds that tool
  -- into the relevance filter instead).
  tool_always_available_off_json TEXT NOT NULL DEFAULT '[]',
  created_at_ms       INTEGER NOT NULL,
  updated_at_ms       INTEGER NOT NULL
);

-- Scheduled prompts: agent- or user-created schedules that fire automated
-- sessions. `schedule_json` holds the ScheduleSpec; `next_run_at_ms` is the
-- armed occurrence (NULL once a 'once' schedule has fired or while disabled).
CREATE TABLE IF NOT EXISTS scheduled_prompts (
  id               TEXT PRIMARY KEY,
  owner_client_id  TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
  title            TEXT NOT NULL,
  instruction      TEXT NOT NULL,
  schedule_json    TEXT NOT NULL,
  run_missed       INTEGER NOT NULL DEFAULT 0,
  enabled          INTEGER NOT NULL DEFAULT 1,
  last_run_at_ms   INTEGER,
  next_run_at_ms   INTEGER,
  created_at_ms    INTEGER NOT NULL,
  updated_at_ms    INTEGER NOT NULL
);

-- Per-client settings overlay. Sparse: only the non-default values a client
-- set are stored, one row per key. The effective config the core applies for a
-- client is the shared core settings.json overlaid with this client's rows
-- (see services/core-settings.ts loadEffective). Holds only "client-on-core"
-- destination keys (per-client inference knobs); shared-resource keys stay in
-- settings.json. ON DELETE CASCADE reaps a removed/revoked client's overlay.
CREATE TABLE IF NOT EXISTS client_settings (
  client_id   TEXT NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
  key         TEXT NOT NULL,
  value_json  TEXT NOT NULL,             -- JSON-encoded scalar
  PRIMARY KEY (client_id, key)
);

-- Downloads (ported from the existing Rust download manager)
CREATE TABLE IF NOT EXISTS downloads (
  id               TEXT PRIMARY KEY,
  source           TEXT NOT NULL,
  destination      TEXT NOT NULL,             -- 'models'|'binaries'|'extensions'
  rel_path         TEXT NOT NULL,
  abs_path         TEXT NOT NULL,
  filename         TEXT NOT NULL,
  group_id         TEXT NOT NULL,
  size_bytes       INTEGER,
  downloaded_bytes INTEGER NOT NULL DEFAULT 0,
  status           TEXT NOT NULL,             -- Pending|Downloading|Completed|Error|Cancelled
  error            TEXT,
  added_at_ms      INTEGER NOT NULL
);
