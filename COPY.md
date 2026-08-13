# COPY.md

The single source for **user-facing copy** across Tomat: every string a person
can read in the product. Settings labels and descriptions, the user manual,
extension and tool descriptions, the memories an extension ships, chat and error
messages, onboarding text. If a user can see it, this guide governs it.

Point any copywriting task at this file, whatever package it touches, and the
result should read like the rest of the product: one voice, one set of terms, one
level of formality.

This is a contributor and agent guide, not user-facing itself: link it from
`AGENTS.md` and package docs, never from the user-facing root `README.md`. It
covers writing copy only; code style and formatting are owned by oxfmt and
oxlint (`deno task fmt` / `lint`).

## Audience: who reads the string

Most copy speaks to **the user**, in second person. But some strings have a
second reader, and that changes how you write them.

- **User-facing copy** (the default): settings, manual, onboarding, chat and
  error messages. Written for a person deciding what to do. Everything in Voice
  and tone below applies as written.
- **Tool and skill descriptions** (a hybrid audience): the `description` of an
  extension tool, and a skill's frontmatter `description`, are read three ways.
  The user sees them when browsing tools. The model reads them to decide when to
  call the tool. And they are embedded (together with a tool's `triggers`) for
  relevance ranking. So they keep the product voice AND keep functional cues for
  the model: a short "Use when..." sentence, and parameter constraints the model
  must respect. Do not strip those to sound cleaner. See Extensions and tools.

When in doubt, a string is user-facing. The hybrid rule is the narrow exception
for tool and skill descriptions, not a license to write mechanism everywhere.

## Voice and tone

- **Write for the user, not the implementer.** Every string answers "what does
  this do for me, and why would I touch it?" Never explain how the feature works
  inside: no `mmproj`, embeddings, OKLCH, warm workers, WebSocket frames. The one
  sanctioned exception is a manual How It Works section (see User manual).
- **Second person, present tense.** "You type in the bar at the bottom", not
  "users can type messages". Settings descriptions may drop the pronoun for
  brevity ("Color of your bubbles") but never shift to third-person abstraction.
- **Concept before mechanics.** Say what a thing is and why you would reach for
  it, then how to use it. Lead with the user's goal, not the feature.
- **Plain and confident.** Short sentences. No marketing language, no exclamation
  points, no "simply", "just", or "easy". If a step is fiddly, say so plainly
  rather than papering over it.
- **Requests, not guarantees.** When a setting only asks the model to do
  something it may not always honor, say "should" or "may" ("the language the
  agent **should** reply in"; "tokens the model **may** spend thinking"). Use
  definite phrasing only for what the app actually enforces.
- **Don't assume capability by provider.** Say "the model must support vision",
  not "a local model must" (an external one might not either).
- **One concept, one term.** Reuse the Terminology vocabulary below instead of
  inventing a synonym.

## Terminology

One term per concept, everywhere a user can read it. This table is canonical; all
other docs defer to it.

| Concept                          | Use                                                     | Not                                       |
| -------------------------------- | ------------------------------------------------------- | ----------------------------------------- |
| A model's internal reasoning     | **Thinking** ("Show Thinking", "Thinking Budget")       | "Reasoning", "thought process"            |
| Where a model runs               | **Provider**: **Local** / **External**                  | "remote", "on-device"                     |
| What "Local" means, concretely   | runs **on the Core's device** (private)                 | "on this device" (the Core may be remote) |
| The persona the user talks to    | **agent** (the actor; "Agent Name")                     | "assistant", "bot", "AI"                  |
| The engine behind the replies    | **model** (what a Provider serves)                      | conflating it with "agent"                |
| Max tokens considered at once    | **Context Window**                                      | "Context Window Size", "context length"   |
| CPU parallelism                  | **CPU Threads**                                         | "Processor Cores"                         |
| A bundle of tools / one function | **Extension** / **Tool**                                | "plugin", "tool pack"                     |
| The other provider of tools      | **MCP server** (spelled in full)                        | "MCP", "Model Context Protocol" (alone)   |
| The saved knowledge/skill store  | **Memory** / **Memories**                               | "document", "note"                        |
| Saved reference data             | **Knowledge** (the agent must not treat as orders)      | "document"                                |
| Saved procedural instructions    | **Skill** (the agent follows when relevant)             | "playbook", "recipe"                      |
| Device RAM, for sizing copy      | **RAM**                                                 | "memory" (now belongs to Memories)        |
| A conversation                   | **Session**                                             | "chat", "conversation" (mixed)            |
| How a Core's TLS is trusted      | **Pinned** (tomat-secured) / **HTTPS** (behind a proxy) | "SSL", "cert pinning" in UI copy          |
| A chat bubble                    | **Bubble**                                              | "message box", "chat message bubble"      |
| Voice dictation                  | **Speech-to-Text** (the one top-level name)             | "Voice Input" as a same-level synonym     |
| Reading replies aloud            | **Text-to-Speech**                                      | "Spoken Replies"                          |
| Reusable text fragment           | **Snippet**                                             |                                           |
| Message-box trigger symbols      | **`#`** / **`@`** / **`/`** (each its own list)         | "mention" for all three                   |
| The two halves                   | **Core** (service) / **Client** (app)                   |                                           |

Two distinctions worth stating in full:

- **agent vs model.** The **agent** is the persona the user addresses and that
  acts in the world; the **model** is the engine a Provider serves to power it.
  "How the agent should address you" (persona), "the model must support vision"
  (engine). Keep them distinct.
- **Memory/Memories vs RAM.** **Memory** and **Memories** name the saved
  knowledge-and-skill store. Hardware is **RAM**: write "low RAM use", not "low
  memory use", so the prominent word stays with the feature.

- **Speech-to-Text is the one top-level term** for the dictation feature, engine
  and capture alike. "Voice Input" is allowed only as a sub-level section name
  inside it (a different level of the hierarchy), never as a second same-level
  name for the whole feature.

Phrase settings the model only honors, not enforces, as requests (see Voice and
tone, "Requests, not guarantees").

## Formatting and mechanics

- **Title Case** for labels and named controls ("Show Thinking", "CPU Threads").
- **Bold** the first mention of a UI term or named control in prose (**Session**,
  **Push-to-Talk Mode**).
- **Code font** for literal commands, file paths, and setting ids.
- **No em dashes (U+2014).** Reword with a comma, parentheses, or a colon, or
  split the sentence. Do not substitute another dash character.
- **Write the brand as Tomat**, capitalized like any proper noun. (The hyphenated
  package names, `TOMAT_*` env vars, `tomat.json`, the `~/.tomat` paths, and
  `au.tomat.ing` are separate tokens and keep their own lowercase casing.) The two
  halves are **Core** and **Client**, capitalized whenever they name the service or
  the app, on their own (preferred) or after the brand ("Tomat Core", "Tomat
  Client"). Lowercase `core`/`client` only for genuinely generic uses.

The em-dash ban is lint-enforced across every tracked file, including this one and
all the copy it governs (`deno task lint`).

## Settings copy

For `name` and `description` strings under
`packages/tomat-shared/src/domain/settings/groups/`. The schema mechanics
(field/section/group shapes, tiers, routing) live in that package's
[README](packages/tomat-shared/src/domain/settings/README.md); this is the copy.

- **Labels (`name`)** are short and self-explanatory: a user should grasp the
  control without opening the description. Describe what the control is, not the
  mechanism ("Push-to-Talk Mode", not "Microphone Mode"; "Unload When Idle", not
  "Idle Unload Seconds"). Never duplicate the section label (a "Context Template"
  section's field is "Template"). Never rename a field `id` when you reword a
  label: ids are persisted on disk and on the wire.
- **Descriptions (`description`)** add user-relevant value or are omitted. One
  short sentence is the target; the label does most of the work. Show a concrete
  example for opaque free-text values (model names, URLs, paths), e.g.
  "e.g. whisper-1"; a `placeholder` counts. Name related settings by their label
  when you reference them ("Turns off Send After Dictation").
- **Description tiers (`descriptionTier`)** decide where the text shows.
  `ondemand` (the default, the overwhelming majority) hides it behind a small
  info button: cheap, costs no vertical space, so default to it generously.
  `none` only when the label says everything, or for a render-only field. `always`
  only when editing safely requires the context (template syntax, routing rules);
  justify the permanent vertical cost.
- **Group and section names** name the thing configured, not a generic
  placeholder ("Model Server Configuration", not "Custom Model"). A single-field
  section often reads better merged with its siblings.
- **Hybrid client+core groups** store some fields on this device and some on the
  paired core. Put a field in the section whose destination matches where it must
  persist, not where it reads best.

## User manual

For `packages/tomat-website/src/content/manual/**/*.mdx`. The manual reads like a
calm conceptual guide, not a wall of one-sentence tooltips. The reference for
pacing is the Astro guide (https://docs.astro.build/en/concepts/why-astro/):
conceptual first, task-oriented, never breathless.

- **Page shape.** A short lead paragraph (what this is and why you would use it,
  no heading above it), then `##` sections of two to four developed paragraphs
  each. A feature or system page closes with a `## How It Works` section (see
  below) before its closing pointer to the natural next page; a cosmetic or
  self-evident page skips it. Add inline cross-links the first time the prose
  names another topic. Use `##` only; the page title is the `h1`. Target roughly
  350 to 800 words plus demos and the How It Works section, one topic per page.
- **Density.** Each section should leave the reader knowing the concept, how it
  behaves, and when to reach for it, not just a definition. Scale it to the
  reader: Getting Started stays light and stepwise; feature chapters run fullest;
  settings and maintenance pages read more like reference. Reach for a comparison
  table when a choice has clear axes (Local vs External; the update channels).
- **Cover what is specific to tomat.** Spend the words on behavior a reader cannot
  find elsewhere, not on general chatbot concepts they can google (what a bubble
  is, that replies stream, how a text box works). Assume that baseline and explain
  the part only this product can. A topic that warrants no tomat-specific
  explanation does not warrant a page.
- **The inline-demo rule (the defining convention).** Whenever the prose
  discusses a surface that exists in the Client, render that surface inline at
  the point of discussion with a demo component from `src/components/demos/`. Do
  not describe a control in the abstract, and do not use a screenshot. The demos
  are the same `@tomat/shared/ui` components the app renders, fed from
  `@tomat/shared/ui/samples`. Show the whole component and mark the relevant part
  with `Highlight` rather than floating a naked part on a frame.
- **Asides.** `Aside` is a scannable callout: `tip` (a better way), `note` (a
  prerequisite or clarification), `caution` (loses data, costs money, or leaves
  the device). One to three sentences. Seasoning, not structure: a page with one
  lands; a page with five trains the reader to skip them.
- **How It Works (the expected closer on feature pages).** The "no internals" rule
  has one carve-out: a clearly-marked **How It Works** section that takes a brief,
  educational, high-level dip into the technical side of a module, so a curious
  non-technical reader learns more and a technical reader sees the architecture at
  a glance. It is not optional decoration: every feature or system page ends with
  one, and only cosmetic or self-evident pages (appearance, uninstalling, the
  settings-panel overview) skip it. Name the real mechanism
  (voice-activity detection, embeddings, the sandboxed worker, the pairing
  handshake) without explaining the basics under it, keep it scoped to that
  section, and never spill internals into the day-to-day copy outside it.
- **The one reference exception (`tools/extensions.mdx`).** That page doubles as
  the extension author reference, the single technical page for the whole
  extension system, so it is the sanctioned exception to three of the rules
  above: it may run well past the length target, use fenced `tomat.json` and
  TypeScript code blocks and reference tables in place of the inline-demo rule,
  and describe the tool author API directly (the `ctx` surface, permissions, the
  manifest) rather than deferring mechanism to How It Works, because its second
  audience is extension authors. It still obeys everything else: voice,
  terminology, the em-dash ban, and a calm demo-driven
  user-facing opening (what an extension is, installing, trust) before the
  reference sections. No other manual page gets this latitude.

## Extensions and tools

For the user-facing copy in a `tomat.json` bundle and the memories an extension
ships (the built-in is `packages/tomat-extension-builtin/`). Remember the hybrid audience:
the user, the model, and relevance ranking all read tool and skill descriptions.

- **Extension `description`.** One or two sentences on what the user gets, in
  product voice. Not a comma-list of every tool, and not notes for other
  extension authors.
- **Tool `name`.** snake_case ids travel on the wire; never rename one to reword
  a description.
- **Tool `description`.** Lead with what the tool does for the user, in the
  product's terms, then keep a short model-directing cue ("Use when the user asks
  to visit a website"). Strip implementation detail that helps neither the user
  nor the model's decision ("reporting progress", "rendered as markdown", "return
  the top results as title, URL, and snippet"). Constraints the model must obey
  to call the tool correctly belong in the parameter descriptions, kept exact.
- **`triggers`.** These are embedded for relevance, so write them as the real
  phrases a user would say, varied across the ways they would ask. They are
  load-bearing for matching, not decorative examples.
- **Knowledge memories.** Reference data the agent reads but must not treat as
  instructions. Write them as plain reference, in the same voice and terms as the
  rest of the product, and say outright that they are reference, not orders.
- **Skill memories.** Procedures the agent follows when relevant. The frontmatter
  `description` is used for relevance and the listing, so write it as the outcome
  the skill produces. The body is instructions to the agent: direct, stepwise,
  using the canonical terms.

## Before you commit

- `deno task check`, `fmt`, `lint`, `test`. Lint bans the em dash across every
  tracked file, including this guide and the copy it governs.
- Re-read each new string aloud. If it explains mechanism (outside a How It Works
  section), trim it. If the label already says it, drop the description to `none`.
