/**
 * The inline form for an MCP prompt's arguments. When the user picks an
 * argument-taking `/prompt` from the "/" menu, the composer pauses on this form
 * (rendered through the SAME AskUserFormView a tool's `ctx.askUser()` uses, so
 * it looks identical) to collect the arguments before the reference is inserted.
 *
 * Each argument is a plain-text question; on submit the filled values are handed
 * back to the host, which stashes them and sends the turn immediately (the
 * `/prompt` token is already in the composer, so the send resolves it with these
 * args).
 */

import type { McpPrompt } from "@loca/shared";
import type { AskUserQuestion } from "@loca/shared";
import type { DraftAnswer } from "@loca/shared/ui/components/chat/userinput/AskUserFormView.svelte";

function blankDraft(): DraftAnswer {
  return { text: "", picks: [], freestyleActive: false, rows: [] };
}

type PromptButton = {
  icon?: string;
  label: string;
  title: string;
  disabled?: boolean;
  onClick: () => void;
};

export class McpPromptArgs {
  // The prompt whose arguments are being collected, plus the callback that
  // receives the filled values on submit; null when no form is active.
  private state = $state<{
    prompt: McpPrompt;
    onComplete: (prompt: McpPrompt, args: Record<string, string>) => void;
  } | null>(null);
  // Working copy of the answers, indexed by argument number.
  drafts = $state<Record<number, DraftAnswer>>({});

  get active(): boolean {
    return this.state !== null;
  }

  /** Open the form for `prompt`; `onComplete` fires with the filled args on
   *  submit. Only call for a prompt that declares arguments. */
  begin(
    prompt: McpPrompt,
    onComplete: (prompt: McpPrompt, args: Record<string, string>) => void,
  ): void {
    const next: Record<number, DraftAnswer> = {};
    for (let i = 0; i < prompt.arguments.length; i++) next[i] = blankDraft();
    this.drafts = next;
    this.state = { prompt, onComplete };
  }

  cancel = (): void => {
    this.state = null;
    this.drafts = {};
  };

  // One plain-text question per argument. A required argument is marked in the
  // label; the description (if any) rides on a second line.
  get questions(): AskUserQuestion[] {
    const p = this.state?.prompt;
    if (!p) return [];
    return p.arguments.map((a) => {
      const label = a.required ? `${a.name} (required)` : a.name;
      return { question: a.description ? `${label}\n${a.description}` : label };
    });
  }

  setText = (idx: number, text: string): void => {
    this.drafts[idx] = { ...(this.drafts[idx] ?? blankDraft()), text };
  };

  get canSubmit(): boolean {
    const p = this.state?.prompt;
    if (!p) return false;
    return p.arguments.every(
      (a, i) => !a.required || (this.drafts[i]?.text.trim().length ?? 0) > 0,
    );
  }

  submit = (): void => {
    const s = this.state;
    if (!s || !this.canSubmit) return;
    const args: Record<string, string> = {};
    s.prompt.arguments.forEach((a, i) => {
      const v = this.drafts[i]?.text.trim() ?? "";
      if (v) args[a.name] = v;
    });
    const { prompt, onComplete } = s;
    this.cancel();
    onComplete(prompt, args);
  };

  // Cancel / Send, surfaced as composer buttons like the permission pair.
  // Submitting sends the turn straight away, so the commit reads "Send" (and
  // wears the composer's send icon) rather than "Insert".
  get actions(): PromptButton[] {
    return [
      {
        icon: "i-material-symbols-close-rounded",
        label: "Cancel",
        title: "Cancel this prompt",
        onClick: this.cancel,
      },
      {
        icon: "i-material-symbols-send-outline-rounded",
        label: "Send",
        title: "Send with this prompt",
        disabled: !this.canSubmit,
        onClick: this.submit,
      },
    ];
  }
}
