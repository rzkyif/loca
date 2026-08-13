<script lang="ts">
  import { onDestroy } from "svelte";
  import AttachmentList from "../AttachmentList.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import { streamingState } from "../../../state";
  import { getTextContent, type MessageContent, type MessagePart } from "$lib/util/types";
  import { showUserMessageMenu } from "$lib/chat/message-menu";

  let {
    content,
    editing = false,
    onStartEdit,
    onStopEdit,
    onEdit,
    onReprocess,
    onDelete,
  } = $props<{
    content: MessageContent;
    editing?: boolean;
    onStartEdit?: () => void;
    onStopEdit?: () => void;
    onEdit?: (newContent: MessageContent) => void;
    onReprocess?: () => void;
    onDelete?: () => void;
  }>();

  let editText = $state("");
  let editTimeout: ReturnType<typeof setTimeout> | null = null;

  onDestroy(() => {
    if (editTimeout) {
      clearTimeout(editTimeout);
      editTimeout = null;
    }
  });

  let displayText = $derived(getTextContent(content));

  // Extract document and image parts for display
  let attachments = $derived(
    typeof content === "string"
      ? []
      : (content as MessagePart[]).filter(
          (p) =>
            p.type === "document" ||
            p.type === "image_url" ||
            p.type === "document_file" ||
            p.type === "image_file",
        ),
  );

  // Local copy of attachments for editing (so removals are tracked)
  let editAttachments = $state<MessagePart[]>([]);

  // Sync editText and editAttachments with content when entering edit mode
  $effect(() => {
    if (editing) {
      editText = displayText;
      editAttachments = [...attachments];
    }
  });

  // Editing is owned by the parent (only one user message edits at a time).
  // When the parent flips us off (sibling double-click, new turn arrives, or
  // explicit Stop), flush any debounced edit so typed changes aren't lost
  // before the textarea unmounts.
  $effect(() => {
    if (!editing && editTimeout) {
      clearTimeout(editTimeout);
      editTimeout = null;
      emitEdit();
    }
  });

  /** Build updated MessageContent from current edit state */
  function buildEditContent(): MessageContent {
    const trimmed = editText.trim();
    if (editAttachments.length === 0) {
      return trimmed;
    }
    const parts: MessagePart[] = [];
    if (trimmed) {
      parts.push({ type: "text", text: trimmed });
    }
    parts.push(...editAttachments);
    return parts;
  }

  function emitEdit() {
    if (!onEdit) return;
    const newContent = buildEditContent();
    const isEmpty = typeof newContent === "string" ? !newContent : newContent.length === 0;
    if (isEmpty) {
      onEdit(newContent);
      return;
    }
    const oldText = displayText;
    const newText = editText.trim();
    const attachmentsChanged = editAttachments.length !== attachments.length;
    if (newText !== oldText || attachmentsChanged) {
      onEdit(newContent);
    }
  }

  function handleEditInput() {
    if (editTimeout) clearTimeout(editTimeout);
    editTimeout = setTimeout(() => emitEdit(), 1000);
  }

  function handleEditKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      editText = displayText;
      editAttachments = [...attachments];
      if (editTimeout) clearTimeout(editTimeout);
    }
  }

  function removeAttachment(index: number) {
    editAttachments = editAttachments.filter((_, i) => i !== index);
    if (editTimeout) clearTimeout(editTimeout);
    setTimeout(() => emitEdit(), 0);
  }

  function toggleEdit() {
    if (editing) {
      onStopEdit?.();
    } else {
      onStartEdit?.();
    }
  }

  // Right-click (desktop) and long-press (touch) open the same message menu.
  function openMessageMenu() {
    void showUserMessageMenu({
      editing,
      isStreaming: streamingState.isActive,
      onToggleEdit: onEdit ? toggleEdit : undefined,
      onReprocess,
      onDelete,
    });
  }
</script>

<UserMessageView
  text={displayText}
  editable={editing}
  active={editing}
  ondblclick={() => {
    if (!editing && onEdit) onStartEdit?.();
  }}
  oncontextmenu={(e) => {
    e.preventDefault();
    openMessageMenu();
  }}
  onlongpress={openMessageMenu}
>
  {#snippet editBody()}
    <div class="grid w-fit min-w-0 max-w-[calc(100vw-135px)] overflow-clip">
      <span
        class="invisible whitespace-pre-wrap break-words wrap-break-word col-start-1 row-start-1 pointer-events-none"
        >{editText + "\u200b"}</span
      >
      <textarea
        bind:value={editText}
        oninput={handleEditInput}
        onkeydown={handleEditKeydown}
        class="loca-no-focus-ring col-start-1 row-start-1 bg-transparent outline-none min-w-0 w-full max-w-full overflow-hidden resize-none whitespace-pre-wrap break-words"
        rows="1"
        cols="1"
      ></textarea>
    </div>
  {/snippet}
  {#snippet attachmentRow()}
    <AttachmentList
      parts={editing ? editAttachments : attachments}
      editable={editing}
      onRemove={removeAttachment}
    />
  {/snippet}
</UserMessageView>
