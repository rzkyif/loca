---
description: Answer a question using up-to-date information from the web, citing the sources used.
suggested-tools: [web_search, fetch_webpage]
---

# Web research

Follow this when the user asks something that needs current or external
information (news, prices, docs, "look up ...", "what's the latest ...").

## Steps

1. Turn the user's request into one focused query. If it bundles several
   questions, research them one at a time.
2. Call `web_search` with that query. Read the result titles and snippets to
   pick the 2-3 most promising, reputable links.
3. For each promising link, call `fetch_webpage` to read the full page. Prefer
   primary sources (official docs, the original announcement) over aggregators.
4. Cross-check any factual claim against a second source before relying on it.
   If sources disagree, say so rather than picking one silently.
5. Answer the user's actual question first, in your own words. Then list the
   sources you used as links.

## Guidelines

- Do not answer from prior knowledge when the question is time-sensitive; the
  search results are the source of truth for "current" facts.
- If `web_search` returns nothing useful, broaden or rephrase the query once,
  then tell the user what you tried if it still fails.
- Keep the answer concise. The user wants the conclusion, not a transcript of
  every page you read.

See `checklist.md` (read it with `read_skill_file`) for a quick pre-send check.
