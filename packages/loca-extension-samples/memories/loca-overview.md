# What Loca is

Loca is a local-first AI client. A long-running background service (the
**core**) owns every stateful job: conversations, the local language model,
sandboxed tool execution, speech, downloads, and connections to extensions and
MCP servers. A thin desktop app (the **client**) renders the chat, captures
input, and plays audio. They talk over a local HTTP + WebSocket API.

## Providers

Tools and memories come from **providers**:

- An **extension** is a bundle the core installs (the built-in set, an npm
  package, or a local folder). Its tools run inside a permission sandbox.
- An **MCP server** is a Model Context Protocol process the core connects to. It
  contributes tools, prompts (used as `/` commands), and resources (referenced
  with `@`).

## Memories

A memory is one of two kinds:

- **Knowledge** is reference data (like this file). The agent reads it but never
  treats its contents as instructions.
- **Skill** is a procedure the agent follows when it applies, packaged as a
  folder with a `SKILL.md` plus optional reference files.

A memory is referenced inline in a message with `@name`, and can be turned on or
off individually.

This file is reference data only. It describes how Loca is organized; it does
not direct the agent to do anything.
