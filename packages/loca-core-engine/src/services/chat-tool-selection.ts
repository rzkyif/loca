// Tool selection for a turn: the LLM-exposure gate, the relevance filter
// pipeline (cosine phase 1, optional LLM phase 2), the final cap, and the
// always-available + MCP tool offering. Produces the provider `tools` array,
// the reusable query embedding, and the tool_filter bubble for the caller to
// finalize (so message ordering stays with the orchestrator).

import type OpenAI from "openai";
import type {
  Tool,
  ToolDescriptor,
  ToolFilterEntryPersisted,
  ToolFilterMessage,
} from "@loca/shared";
import { errMessage, permissionKey } from "@loca/shared";
import { boolSetting, numSetting } from "./settings-access.ts";
import { embed } from "./embedding.ts";
import { cosineNormalized, toolEmbedText, toolNameMentioned } from "./relevance.ts";
import { toolFilter } from "./tool-filter.ts";
import { resolveEndpoint } from "./endpoint-resolver.ts";
import { thinkingBudget } from "./thinking-budget.ts";
import { host } from "../platform/runtime.ts";
import { getLogger } from "../platform/log.ts";
import { newMessageId } from "../platform/ids.ts";

const log = getLogger("chat");

// Cosine cutoff for folding a non-always-available MCP tool into a turn: with no
// trigger phrases, an MCP tool is offered when the query names it OR its name +
// description embedding is at least this similar to the query. A conservative
// floor so an irrelevant server tool isn't force-included every turn; tune if
// tools surface too eagerly or too rarely.
const MCP_RELEVANCE_FLOOR = 0.25;

// LLM-exposure gate: a tool reaches the model only when its extension is
// 'installed' (not 'downloaded'/'drift'), the tool is enabled, AND no
// non-optional required permission is denied. A permission without a grant
// row behaves as 'ask': the tool runs and Deno prompts at the moment of
// access, so only an explicit denial withholds the tool here.
export function toolExposable(t: Tool): boolean {
  const denied = new Set(t.grants.filter((g) => g.state === "denied").map((g) => g.permissionKey));
  return t.requiredPermissions.every((d) => d.optional || !denied.has(permissionKey(d)));
}

export function enabledToolsByName(): Map<string, { extensionId: string; toolId: string }> {
  const out = new Map<string, { extensionId: string; toolId: string }>();
  for (const t of host().tools?.installedExtensionTools() ?? []) {
    if (t.enabled && toolExposable(t)) {
      out.set(t.name, { extensionId: t.extensionId, toolId: t.id });
    }
  }
  // Enabled MCP tools resolve to their server id, so executeToolCall routes
  // them through the MCP client. Extension tools win a name collision (set
  // first), matching the offering order.
  for (const t of host().tools?.listMcpTools() ?? []) {
    if (t.enabled && !out.has(t.name)) {
      out.set(t.name, { extensionId: t.extensionId, toolId: t.id });
    }
  }
  return out;
}

export function listEnabledTools(): Tool[] {
  const out: Tool[] = [];
  for (const t of host().tools?.installedExtensionTools() ?? []) {
    if (t.enabled && toolExposable(t)) out.push(t);
  }
  return out;
}

export interface ToolSelection {
  tools: OpenAI.Chat.Completions.ChatCompletionTool[];
  // Query embedding computed by the tool filter, reused by the memory relevance
  // injection so the turn embeds the query at most once. Undefined when the
  // filter didn't run (tools disabled, no query, or filtering skipped).
  queryVector: Float32Array | undefined;
  // The tool_filter bubble, for the caller to `writer.finalize`. Null when the
  // tool path is gated off (no bubble on a no-tools turn).
  filterMessage: ToolFilterMessage | null;
}

/** Build the provider tool list for a turn. The caller finalizes
 *  `filterMessage` (when non-null) so the bubble lands at the turn's cursor. */
export async function buildToolList(opts: {
  settings: Record<string, unknown>;
  route: "default" | "secondary";
  streamId: string;
  signal: AbortSignal;
  queryText: string | undefined;
}): Promise<ToolSelection> {
  const { settings, route, streamId, signal, queryText } = opts;

  // Tool path is gated on tools.enabled. If filtering is off, every enabled
  // tool is sent. If on, run phase 1 (cosine) and optionally phase 2 (LLM
  // relevance), then apply tools.maxTools as a final cap and add
  // always-available tools.
  let toolList: OpenAI.Chat.Completions.ChatCompletionTool[] = [];
  let queryVector: Float32Array | undefined;
  if (!boolSetting(settings, "tools.enabled", false)) {
    return { tools: toolList, queryVector, filterMessage: null };
  }

  // The filter bubble is emitted only once the pipeline finishes (end state:
  // complete or error). The in-flight period is covered by the synthetic
  // loading bubble, which renders until the first model delta.
  const filterMsg: ToolFilterMessage = {
    id: newMessageId(),
    ord: -1,
    role: "tool_filter",
    status: "complete",
    createdAtMs: Date.now(),
  };
  try {
    const allEnabled = listEnabledTools();
    const totalCount = allEnabled.length;
    const filteringEnabled = boolSetting(settings, "tools.filteringEnabled", true);
    const minToolsToFilter = numSetting(settings, "tools.filteringMinTools", 0);
    const maxTools = numSetting(settings, "tools.maxTools", 30);
    const alwaysAvailableEnabled = boolSetting(settings, "tools.alwaysAvailableEnabled", true);
    const secondPassEnabled = boolSetting(settings, "tools.secondPassEnabled", true);

    // Skip the filter entirely when disabled or when total tool count
    // is below the user's minimum threshold.
    const shouldFilter =
      filteringEnabled &&
      totalCount > 0 &&
      (minToolsToFilter === 0 || totalCount >= minToolsToFilter);

    let chosenTools: Array<{ toolId: string }>;
    let phase1Entries: ToolDescriptor[] = [];
    let phase2Entries: ToolDescriptor[] | undefined;
    let alwaysEntries: ToolDescriptor[] = [];
    let nameMatchedEntries: ToolDescriptor[] = [];

    if (!shouldFilter || !queryText) {
      const capped: Array<{ toolId: string }> = allEnabled.slice(0, maxTools).map((t) => ({
        toolId: t.id,
      }));
      // maxTools can cut off always-available tools that sort past the cap;
      // append them so their exposure doesn't depend on the filter toggle
      // (the filtered branch below also always adds always-available tools).
      if (alwaysAvailableEnabled) {
        const seen = new Set(capped.map((c) => c.toolId));
        for (const t of allEnabled) {
          if (t.alwaysAvailable && !seen.has(t.id)) {
            capped.push({ toolId: t.id });
            seen.add(t.id);
          }
        }
      }
      chosenTools = capped;
    } else {
      const [vector] = await embed([queryText]);
      queryVector = vector;
      const result = toolFilter().phase1(vector, {
        topK: maxTools,
        includeAlwaysAvailable: alwaysAvailableEnabled,
        queryText,
      });
      phase1Entries = result.candidates;
      alwaysEntries = alwaysAvailableEnabled ? result.alwaysAvailable : [];
      nameMatchedEntries = result.nameMatched;
      let candidates = result.candidates;
      if (secondPassEnabled && candidates.length > 0) {
        const phase2Endpoint = await resolveEndpoint(settings, route);
        const filterBudget = thinkingBudget(settings, "tools.filterThinkingBudget");
        candidates = await toolFilter().phase2(
          queryText,
          candidates,
          phase2Endpoint,
          filterBudget,
          signal,
        );
        phase2Entries = candidates;
      }
      const final: Array<{ toolId: string }> = [];
      const seen = new Set<string>();
      // Directly-named tools are force-included first, ahead of the cosine
      // candidates and exempt from the maxTools cap (like always-available
      // tools), so naming a tool by hand always exposes it to the model.
      for (const n of nameMatchedEntries) {
        if (seen.has(n.toolId)) continue;
        final.push({ toolId: n.toolId });
        seen.add(n.toolId);
      }
      for (const c of candidates) {
        if (final.length >= maxTools) break;
        if (seen.has(c.toolId)) continue;
        final.push({ toolId: c.toolId });
        seen.add(c.toolId);
      }
      for (const a of alwaysEntries) {
        if (seen.has(a.toolId)) continue;
        final.push({ toolId: a.toolId });
        seen.add(a.toolId);
      }
      chosenTools = final;
    }

    toolList = chosenTools
      .map((c) => host().tools?.getTool(c.toolId))
      .filter((t): t is NonNullable<typeof t> => t !== undefined)
      // Final exposure gate: the relevance filter's candidate set is not
      // grant-aware, so re-apply enabled + fully-granted here (status is
      // already gated upstream) so an ungranted tool never reaches the model.
      .filter((t) => t.enabled && toolExposable(t))
      .map((t) => ({
        type: "function" as const,
        function: {
          name: t.name,
          description: t.description,
          parameters: t.parameters,
        },
      }));

    filterMsg.phase1 = phase1Entries.map((c) => ({
      toolId: c.toolId,
      name: c.name,
      description: c.description,
      score: c.similarity ?? 0,
    }));
    filterMsg.phase2 = phase2Entries?.map((c) => ({
      toolId: c.toolId,
      name: c.name,
      description: c.description,
    }));
    filterMsg.alwaysAvailable = alwaysEntries.map((a) => ({
      toolId: a.toolId,
      name: a.name,
      description: a.description,
    }));
    filterMsg.nameMatched = nameMatchedEntries.map((n) => ({
      toolId: n.toolId,
      name: n.name,
      description: n.description,
    }));
  } catch (err) {
    // Embedding model not present, etc. This is non-fatal; we just skip tools.
    log.warn(`stream ${streamId}: tool filter failed, skipping tools: ${errMessage(err)}`);
    filterMsg.status = "error";
    filterMsg.errorMessage = errMessage(err);
  }

  // Enabled MCP tools live on their servers, so they aren't in the embedding
  // index. An MCP tool defaults to always-available (offered every turn); when
  // its always-available is turned off it folds into relevance instead: with no
  // trigger phrases, it's offered when the query names it or its name +
  // description embedding clears MCP_RELEVANCE_FLOOR. Guards:
  //  - skip any whose name an extension tool already claims (a duplicate
  //    function name confuses the model, and dispatch resolves the name to
  //    the extension anyway, so the MCP entry would be dead weight), and
  //    likewise skip a second MCP tool of the same name.
  //  - bound the count at `maxTools` so a server with many enabled tools
  //    can't blow the tool/context budget.
  const mcpMaxTools = numSetting(settings, "tools.maxTools", 30);
  const mcpAlwaysAvailableEnabled = boolSetting(settings, "tools.alwaysAvailableEnabled", true);
  const offered = new Set(toolList.map((t) => (t.type === "function" ? t.function.name : "")));
  const mcpEntries: ToolFilterEntryPersisted[] = [];
  for (const t of host().tools?.listMcpTools() ?? []) {
    if (!t.enabled || offered.has(t.name)) continue;
    if (mcpEntries.length >= mcpMaxTools) break;
    const isAlways = t.alwaysAvailable && mcpAlwaysAvailableEnabled;
    // With no query vector (filtering off/skipped) everything is offered,
    // matching the extension path; otherwise a non-always tool must be relevant.
    let include = isAlways || queryVector === undefined;
    if (!include && queryVector) {
      if (toolNameMentioned(queryText ?? "", t.name)) {
        include = true;
      } else {
        try {
          const [vec] = await embed([toolEmbedText(t)]);
          include = cosineNormalized(queryVector, vec) >= MCP_RELEVANCE_FLOOR;
        } catch (err) {
          // Embedding model unavailable: fall back to name-mention only (already
          // checked above), so the tool is simply not offered this turn.
          log.warn(`stream ${streamId}: MCP tool embed failed for ${t.name}: ${errMessage(err)}`);
        }
      }
    }
    if (!include) continue;
    offered.add(t.name);
    mcpEntries.push({ toolId: t.id, name: t.name, description: t.description });
    toolList.push({
      type: "function",
      function: {
        name: t.name,
        description: t.description,
        parameters: t.parameters,
      },
    });
  }

  // MCP tools offered this turn, recorded for the bubble without forcing it
  // visible. toolsSent is set here, after the append, so it counts them: an
  // MCP-only turn would otherwise read as 0 tools sent and suppress the
  // system-prompt tools hint even though the model got tools.
  filterMsg.mcp = mcpEntries;
  filterMsg.toolsSent = toolList.length;

  return { tools: toolList, queryVector, filterMessage: filterMsg };
}
