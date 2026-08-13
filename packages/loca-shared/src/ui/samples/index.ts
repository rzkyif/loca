// Sample prop bundles for every shared `*View`, consumed by the website gallery
// (one card per View per scenario), the showcase stages, and the manual. Each
// View's samples are typed against its props, so a prop rename fails
// svelte-check; snippet props are supplied by the renderer. The `SAMPLES`
// registry is keyed by the exact View component name (`FooView`) so the
// check-view-coverage walker can assert every View has samples here.

export { AGENT_ANSWER, AGENT_REASONING, agentMessageSamples } from "./agent-message.ts";
export { attachmentListSamples } from "./attachment-list.ts";
export { autocorrectAlertSamples } from "./autocorrect-alert.ts";
export { chatShellSamples } from "./chat-shell.ts";
export { colorPickerModalSamples } from "./color-picker-modal.ts";
export { commandPreviewFieldSamples } from "./command-preview-field.ts";
export { messageStackSamples } from "./message-stack.ts";
export { quickSettingsSamples } from "./quick-settings.ts";
export { scheduleConfirmFormSamples } from "./schedule-confirm-form.ts";
export { scheduledPromptDetailSamples } from "./scheduled-prompt-detail.ts";
export { sessionListSamples } from "./session-list.ts";
export { snippetDetailSamples } from "./snippet-detail.ts";
export { updateButtonSamples } from "./update-button.ts";
export { confirmModalSamples } from "./confirm-modal.ts";
export { coreBarSamples } from "./core-bar.ts";
export { deletionsModalSamples } from "./deletions-modal.ts";
export { diffViewSamples } from "./diff-view.ts";
export { downloadRowSamples } from "./download-row.ts";
export { downloadsModalSamples } from "./downloads-modal.ts";
export { errorMessageSamples } from "./error-message.ts";
export { expandableMessageSamples } from "./expandable-message.ts";
export { objectBadgeSamples } from "./object-badge.ts";
export { objectCardSamples } from "./object-card.ts";
export { objectDetailHeaderSamples } from "./object-detail-header.ts";
export { objectDetailScrollSamples } from "./object-detail-scroll.ts";
export { objectManagerSamples } from "./object-manager.ts";
export { coresFieldSamples } from "./cores-field.ts";
export { servicesFieldSamples } from "./services-field.ts";
export { storageFieldSamples } from "./storage-field.ts";
export { extensionDetailSamples } from "./extension-detail.ts";
export { extensionsFieldSamples } from "./extensions-field.ts";
export { toolDetailSamples } from "./tool-detail.ts";
export { toolsFieldSamples } from "./tools-field.ts";
export { mcpDetailSamples } from "./mcp-detail.ts";
export { mcpFieldSamples } from "./mcp-field.ts";
export { memoryDetailSamples } from "./memory-detail.ts";
export { modelPresetFieldSamples } from "./model-preset-field.ts";
export { sttPresetFieldSamples } from "./stt-preset-field.ts";
export { ttsPresetFieldSamples } from "./tts-preset-field.ts";
export { shortcutFieldSamples } from "./shortcut-field.ts";
export { scheduleEditorSamples } from "./schedule-editor.ts";
export { promptButtonsSamples } from "./prompt-buttons.ts";
export { newCoreWizardSamples } from "./new-core-wizard.ts";
export { passwordPromptModalSamples } from "./password-prompt-modal.ts";
export { permissionRequestSamples } from "./permission-request.ts";
export { quickModelBarSamples } from "./quick-model-bar.ts";
export { reasoningTraceSamples } from "./reasoning-trace.ts";
export { relevantMemoriesSamples } from "./relevant-memories.ts";
export { relevantToolsSamples } from "./relevant-tools.ts";
export { sessionBarSamples } from "./session-bar.ts";
export { settingsContentSamples } from "./settings-content.ts";
export { settingsFieldSamples } from "./settings-field.ts";
export { settingsHeaderSamples } from "./settings-header.ts";
export { settingsShellSamples } from "./settings-shell.ts";
export { settingsSidebarSamples } from "./settings-sidebar.ts";
export { shareModalSamples } from "./share-modal.ts";
export { shareTreeSamples } from "./share-tree.ts";
export { snippetAutocompleteSamples } from "./snippet-autocomplete.ts";
export { toolCallSamples } from "./tool-call.ts";
export { userInputSamples } from "./user-input.ts";
export { userMessageSamples } from "./user-message.ts";
export { SAMPLE_FIRST_GROUP, SAMPLE_GROUPS, SAMPLE_VALUES } from "./settings-groups.ts";
export { PRIMITIVE_SAMPLES } from "./primitives.ts";

import { agentMessageSamples } from "./agent-message.ts";
import { attachmentListSamples } from "./attachment-list.ts";
import { autocorrectAlertSamples } from "./autocorrect-alert.ts";
import { chatShellSamples } from "./chat-shell.ts";
import { colorPickerModalSamples } from "./color-picker-modal.ts";
import { commandPreviewFieldSamples } from "./command-preview-field.ts";
import { messageStackSamples } from "./message-stack.ts";
import { quickSettingsSamples } from "./quick-settings.ts";
import { scheduleConfirmFormSamples } from "./schedule-confirm-form.ts";
import { scheduledPromptDetailSamples } from "./scheduled-prompt-detail.ts";
import { sessionListSamples } from "./session-list.ts";
import { snippetDetailSamples } from "./snippet-detail.ts";
import { updateButtonSamples } from "./update-button.ts";
import { confirmModalSamples } from "./confirm-modal.ts";
import { coreBarSamples } from "./core-bar.ts";
import { deletionsModalSamples } from "./deletions-modal.ts";
import { diffViewSamples } from "./diff-view.ts";
import { downloadRowSamples } from "./download-row.ts";
import { downloadsModalSamples } from "./downloads-modal.ts";
import { errorMessageSamples } from "./error-message.ts";
import { expandableMessageSamples } from "./expandable-message.ts";
import { objectBadgeSamples } from "./object-badge.ts";
import { objectCardSamples } from "./object-card.ts";
import { objectDetailHeaderSamples } from "./object-detail-header.ts";
import { objectDetailScrollSamples } from "./object-detail-scroll.ts";
import { objectManagerSamples } from "./object-manager.ts";
import { coresFieldSamples } from "./cores-field.ts";
import { servicesFieldSamples } from "./services-field.ts";
import { storageFieldSamples } from "./storage-field.ts";
import { extensionDetailSamples } from "./extension-detail.ts";
import { extensionsFieldSamples } from "./extensions-field.ts";
import { toolDetailSamples } from "./tool-detail.ts";
import { toolsFieldSamples } from "./tools-field.ts";
import { mcpDetailSamples } from "./mcp-detail.ts";
import { mcpFieldSamples } from "./mcp-field.ts";
import { memoryDetailSamples } from "./memory-detail.ts";
import { modelPresetFieldSamples } from "./model-preset-field.ts";
import { sttPresetFieldSamples } from "./stt-preset-field.ts";
import { ttsPresetFieldSamples } from "./tts-preset-field.ts";
import { shortcutFieldSamples } from "./shortcut-field.ts";
import { scheduleEditorSamples } from "./schedule-editor.ts";
import { promptButtonsSamples } from "./prompt-buttons.ts";
import { newCoreWizardSamples } from "./new-core-wizard.ts";
import { passwordPromptModalSamples } from "./password-prompt-modal.ts";
import { permissionRequestSamples } from "./permission-request.ts";
import { quickModelBarSamples } from "./quick-model-bar.ts";
import { reasoningTraceSamples } from "./reasoning-trace.ts";
import { relevantMemoriesSamples } from "./relevant-memories.ts";
import { relevantToolsSamples } from "./relevant-tools.ts";
import { sessionBarSamples } from "./session-bar.ts";
import { settingsContentSamples } from "./settings-content.ts";
import { settingsFieldSamples } from "./settings-field.ts";
import { settingsHeaderSamples } from "./settings-header.ts";
import { settingsShellSamples } from "./settings-shell.ts";
import { settingsSidebarSamples } from "./settings-sidebar.ts";
import { shareModalSamples } from "./share-modal.ts";
import { shareTreeSamples } from "./share-tree.ts";
import { snippetAutocompleteSamples } from "./snippet-autocomplete.ts";
import { toolCallSamples } from "./tool-call.ts";
import { userInputSamples } from "./user-input.ts";
import { userMessageSamples } from "./user-message.ts";

/** View component name -> its named sample scenarios. The check-view-coverage
 *  walker asserts every `*View.svelte` appears as a key here. */
export const SAMPLES = {
  AgentMessageView: agentMessageSamples,
  AttachmentListView: attachmentListSamples,
  AutocorrectAlertView: autocorrectAlertSamples,
  ChatShellView: chatShellSamples,
  ColorPickerModalView: colorPickerModalSamples,
  CommandPreviewFieldView: commandPreviewFieldSamples,
  ConfirmModalView: confirmModalSamples,
  MessageStackView: messageStackSamples,
  QuickSettingsView: quickSettingsSamples,
  ScheduleConfirmFormView: scheduleConfirmFormSamples,
  ScheduledPromptDetailView: scheduledPromptDetailSamples,
  SessionListView: sessionListSamples,
  SnippetDetailView: snippetDetailSamples,
  UpdateButtonView: updateButtonSamples,
  CoreBarView: coreBarSamples,
  DeletionsModalView: deletionsModalSamples,
  DiffView: diffViewSamples,
  DownloadRowView: downloadRowSamples,
  DownloadsModalView: downloadsModalSamples,
  ErrorMessageView: errorMessageSamples,
  ExpandableMessageView: expandableMessageSamples,
  ObjectBadgeView: objectBadgeSamples,
  ObjectCardView: objectCardSamples,
  ObjectDetailHeaderView: objectDetailHeaderSamples,
  ObjectDetailScrollView: objectDetailScrollSamples,
  ObjectManagerView: objectManagerSamples,
  CoresFieldView: coresFieldSamples,
  ServicesFieldView: servicesFieldSamples,
  StorageFieldView: storageFieldSamples,
  ExtensionDetailView: extensionDetailSamples,
  ExtensionsFieldView: extensionsFieldSamples,
  ToolDetailView: toolDetailSamples,
  ToolsFieldView: toolsFieldSamples,
  McpDetailView: mcpDetailSamples,
  McpFieldView: mcpFieldSamples,
  MemoryDetailView: memoryDetailSamples,
  ModelPresetFieldView: modelPresetFieldSamples,
  SttPresetFieldView: sttPresetFieldSamples,
  TtsPresetFieldView: ttsPresetFieldSamples,
  ShortcutFieldView: shortcutFieldSamples,
  ScheduleEditorView: scheduleEditorSamples,
  PromptButtonsView: promptButtonsSamples,
  NewCoreWizardView: newCoreWizardSamples,
  PasswordPromptModalView: passwordPromptModalSamples,
  PermissionRequestView: permissionRequestSamples,
  QuickModelBarView: quickModelBarSamples,
  ReasoningTraceView: reasoningTraceSamples,
  RelevantMemoriesView: relevantMemoriesSamples,
  RelevantToolsView: relevantToolsSamples,
  SessionBarView: sessionBarSamples,
  SettingsContentView: settingsContentSamples,
  SettingsFieldView: settingsFieldSamples,
  SettingsHeaderView: settingsHeaderSamples,
  SettingsShellView: settingsShellSamples,
  SettingsSidebarView: settingsSidebarSamples,
  ShareModalView: shareModalSamples,
  ShareTreeView: shareTreeSamples,
  SnippetAutocompleteView: snippetAutocompleteSamples,
  ToolCallView: toolCallSamples,
  UserInputView: userInputSamples,
  UserMessageView: userMessageSamples,
} as const;
