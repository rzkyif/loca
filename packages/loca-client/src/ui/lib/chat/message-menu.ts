/**
 * Builds the native right-click menu shown on chat message bubbles.
 * Different items appear depending on whether the message is from the
 * user or the assistant.
 */

import { type ContextMenuItem, platform } from "$lib/platform";
import { isTauri } from "$lib/util/env";
import { ttsState } from "$stores/tts.svelte";

export type UserMenuCtx = {
  editing: boolean;
  isStreaming: boolean;
  onToggleEdit?: () => void;
  onReprocess?: () => void;
  onDelete?: () => void;
};

export type AgentMenuCtx = {
  messageId: string;
  isStreaming: boolean;
  result: string;
  ttsActive: boolean;
  isSpeakingThis: boolean;
  isSynthesizing: boolean;
  onReprocess?: () => void;
  onDelete?: () => void;
};

export type ReasoningMenuCtx = {
  reasoning: string;
};

// Each menu builds [items, dispatch] where dispatch is a map from item-id
// to the callback that fires. The platform layer no longer exposes
// per-item action closures (the old shape leaked Tauri's `MenuItemOptions`
// into the consumer), so we route via id after popup resolves.
type Dispatch = Record<string, () => void>;

async function popup(items: ContextMenuItem[], dispatch: Dispatch): Promise<void> {
  if (!isTauri()) return;
  const chosen = await platform().menu.showContextMenu(items);
  if (chosen && dispatch[chosen]) dispatch[chosen]();
}

export async function showUserMessageMenu(ctx: UserMenuCtx): Promise<void> {
  const items: ContextMenuItem[] = [];
  const dispatch: Dispatch = {};

  if (ctx.onToggleEdit) {
    items.push({
      id: "toggle-edit",
      label: ctx.editing ? "Stop Editing" : "Edit Message",
    });
    dispatch["toggle-edit"] = () => ctx.onToggleEdit?.();
  }

  if (ctx.onReprocess && !ctx.isStreaming) {
    items.push({ id: "reprocess", label: "Reprocess Response" });
    dispatch["reprocess"] = () => ctx.onReprocess?.();
  }

  if (ctx.onDelete) {
    if (items.length > 0) items.push({ separator: true });
    items.push({ id: "delete", label: "Delete Message" });
    dispatch["delete"] = () => ctx.onDelete?.();
  }

  if (items.length === 0) return;
  await popup(items, dispatch);
}

export async function showAgentMessageMenu(ctx: AgentMenuCtx): Promise<void> {
  const items: ContextMenuItem[] = [];
  const dispatch: Dispatch = {};

  if (ctx.ttsActive) {
    if (ctx.isSpeakingThis) {
      items.push({ id: "tts-stop", label: "Stop Speaking" });
      dispatch["tts-stop"] = () => ttsState.reset();
    } else if (ctx.isSynthesizing) {
      // Disabled, so no dispatch entry needed.
      items.push({
        id: "tts-synthesizing",
        label: "Synthesizing...",
        enabled: false,
      });
    } else {
      items.push({ id: "tts-play", label: "Read Aloud" });
      dispatch["tts-play"] = () => ttsState.replayMessage(ctx.messageId, ctx.result);
    }
  }

  if (ctx.result) {
    if (items.length > 0) items.push({ separator: true });
    items.push({ id: "copy", label: "Copy Result" });
    dispatch["copy"] = () => {
      void navigator.clipboard.writeText(ctx.result);
    };
  }

  const tailIds: string[] = [];
  if (ctx.onReprocess && !ctx.isStreaming) {
    tailIds.push("reprocess");
  }
  if (ctx.onDelete) {
    tailIds.push("delete");
  }
  if (tailIds.length > 0) {
    if (items.length > 0) items.push({ separator: true });
    if (tailIds.includes("reprocess")) {
      items.push({ id: "reprocess", label: "Reprocess Message" });
      dispatch["reprocess"] = () => ctx.onReprocess?.();
    }
    if (tailIds.includes("delete")) {
      items.push({ id: "delete", label: "Delete Message" });
      dispatch["delete"] = () => ctx.onDelete?.();
    }
  }

  if (items.length === 0) return;
  await popup(items, dispatch);
}

export async function showReasoningMessageMenu(ctx: ReasoningMenuCtx): Promise<void> {
  const items: ContextMenuItem[] = [];
  const dispatch: Dispatch = {};

  if (ctx.reasoning) {
    items.push({ id: "copy", label: "Copy Thoughts" });
    dispatch["copy"] = () => {
      void navigator.clipboard.writeText(ctx.reasoning);
    };
  }

  if (items.length === 0) return;
  await popup(items, dispatch);
}
