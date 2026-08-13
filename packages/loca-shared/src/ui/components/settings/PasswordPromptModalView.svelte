<script lang="ts">
  // Presentational admin-password prompt: a title, optional message, a password
  // field, an optional error, and the action buttons. The submit/keychain work
  // and the submitting/error state live in the client wrapper; this is pure.
  import Modal from "../primitives/Modal.svelte";
  import Button from "../primitives/Button.svelte";
  import Input from "../primitives/Input.svelte";
  import FormField from "../primitives/FormField.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  let {
    open = true,
    title,
    message,
    password = $bindable(""),
    submitting = false,
    error = null,
    confirmLabel = "Continue",
    onInput,
    onSubmit,
    onCancel,
    onClose,
  }: {
    open?: boolean;
    title: string;
    message?: string;
    password?: string;
    submitting?: boolean;
    error?: string | null;
    confirmLabel?: string;
    onInput?: (v: string) => void;
    onSubmit?: () => void;
    onCancel?: () => void;
    onClose?: () => void;
  } = $props();

  const noop = (): void => {};
</script>

<Modal {open} onclose={onClose ?? noop} ariaLabel={title}>
  <div class="text-default-800 font-medium">{title}</div>
  {#if message}
    <div class="text-default-600 text-sm whitespace-pre-line">{message}</div>
  {/if}
  <FormField label="Admin password">
    <Input
      type="password"
      value={password}
      ariaLabel="Admin password"
      placeholder="••••••••"
      disabled={submitting}
      oninput={(v) => {
        password = v;
        onInput?.(v);
      }}
      onkeydown={(e) => {
        if (e.key === "Enter") onSubmit?.();
      }}
    />
  </FormField>
  {#if error}
    <ErrorDetailView message={error} />
  {/if}
  <div class="flex items-center justify-end gap-2">
    <Button variant="secondary" disabled={submitting} onclick={onCancel ?? noop}>Cancel</Button>
    <Button variant="primary" loading={submitting} disabled={!password} onclick={onSubmit ?? noop}>
      {confirmLabel}
    </Button>
  </div>
</Modal>
