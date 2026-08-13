<script lang="ts">
  import { errMessage } from "@loca/shared";
  import { passwordPromptState } from "$stores/password-prompt.svelte";
  import PasswordPromptModalView from "@loca/shared/ui/components/settings/PasswordPromptModalView.svelte";

  // Local field state, reset whenever a new request arrives so one prompt's
  // input or error can't leak into the next.
  let password = $state("");
  let submitting = $state(false);
  let error = $state<string | null>(null);
  $effect(() => {
    if (passwordPromptState.pending) {
      password = "";
      submitting = false;
      error = null;
    }
  });

  async function submit() {
    const p = passwordPromptState.pending;
    if (!p || submitting || !password) return;
    submitting = true;
    error = null;
    try {
      await p.onSubmit(password);
      // Success closes the modal. The request resolved its own work.
      passwordPromptState.pending = null;
    } catch (e) {
      // Wrong password (or any failure): stay open and let the user retry.
      error = errMessage(e);
      submitting = false;
    }
  }
</script>

{#if passwordPromptState.pending}
  {@const p = passwordPromptState.pending}
  <PasswordPromptModalView
    title={p.title}
    message={p.message}
    bind:password
    {submitting}
    {error}
    confirmLabel={p.confirmLabel ?? "Continue"}
    onInput={() => (error = null)}
    onSubmit={() => void submit()}
    onCancel={() => passwordPromptState.cancel()}
    onClose={() => passwordPromptState.cancel()}
  />
{/if}
