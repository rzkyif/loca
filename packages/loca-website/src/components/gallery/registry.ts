// Single source of truth for what the component gallery must cover. The lint
// walkers (check-view-coverage, check-component-tiers, check-primitive-coverage)
// parse this file to assert coverage. A shared `*View` or primitive absent here
// fails the build, so the gallery cannot silently drop a component.
//
// The renderer (Gallery.svelte / Primitives.svelte / MobileGallery.svelte) is
// HAND-AUTHORED (one card block per component, since each needs bespoke snippets
// and a natural background), it does not blindly iterate these lists. So listing
// a name here is necessary but not sufficient: check-view-coverage /
// check-primitive-coverage additionally assert the renderer actually references
// each entry, so a registered-but-unrendered component fails too.
//
// Keep each list sorted and complete: every `*View.svelte` under
// @loca/shared/ui/components must appear in GALLERY_VIEWS or EMBEDDED_VIEWS, and
// every primitives/*.svelte in GALLERY_PRIMITIVES.

/** Every shared `*View` rendered by the gallery, keyed by its component name. */
export const GALLERY_VIEWS = [
  "AgentMessageView",
  "AttachmentListView",
  "AutocorrectAlertView",
  "ChatShellView",
  "ColorPickerModalView",
  "CommandPreviewFieldView",
  "ConfirmModalView",
  "CoreBarView",
  "CoresFieldView",
  "DeletionsModalView",
  "DownloadsModalView",
  "ErrorMessageView",
  "ExpandableMessageView",
  "ExtensionDetailView",
  "ExtensionsFieldView",
  "McpDetailView",
  "McpFieldView",
  "MemoryDetailView",
  "MessageStackView",
  "ModelPresetFieldView",
  "NewCoreWizardView",
  "ObjectBadgeView",
  "ObjectCardView",
  "ObjectDetailHeaderView",
  "ObjectDetailScrollView",
  "ObjectManagerView",
  "PasswordPromptModalView",
  "PermissionRequestView",
  "PromptButtonsView",
  "QuickSettingsView",
  "RelevantMemoriesView",
  "RelevantToolsView",
  "SessionBarView",
  "SessionListView",
  "SettingsFieldView",
  "SettingsShellView",
  "ServicesFieldView",
  "ShareModalView",
  "ShortcutFieldView",
  "ScheduleEditorView",
  "ScheduleConfirmFormView",
  "ScheduledPromptDetailView",
  "SnippetAutocompleteView",
  "SnippetDetailView",
  "StorageFieldView",
  "SttPresetFieldView",
  "ToolDetailView",
  "ToolsFieldView",
  "TtsPresetFieldView",
  "ToolCallView",
  "UpdateButtonView",
  "UserInputView",
  "UserMessageView",
] as const;

// Views that earn coverage transitively, not from a card of their own: each is a
// pure structural sub-piece always rendered inside the parent named here, and the
// parent's card already shows it in a representative state, so a dedicated card
// would be a redundant duplicate. check-view-coverage exempts these from the
// GALLERY_VIEWS card requirement and instead asserts the parent is galleried,
// rendered, and actually renders the child (so it cannot silently disappear). The
// askuser question sub-views are covered by the analogous AskUserFormView rule in
// the walker. Keep the keys sorted.
export const EMBEDDED_VIEWS: Record<string, string> = {
  // child View -> parent View whose card renders it
  AskUserFormView: "UserInputView",
  DiffView: "ToolCallView",
  DownloadRowView: "DownloadsModalView",
  ErrorDetailView: "ErrorMessageView",
  QuickModelBarView: "UserInputView",
  QuickSettingsSectionView: "QuickSettingsView",
  ReasoningTraceView: "AgentMessageView",
  SettingsContentView: "SettingsShellView",
  SettingsHeaderView: "SettingsShellView",
  SettingsSidebarView: "SettingsShellView",
  ShareTreeView: "ShareModalView",
};

/** Every shared primitive (A0) rendered by the gallery, keyed by component name. */
export const GALLERY_PRIMITIVES = [
  "ActionSheet",
  "Alert",
  "Bubble",
  "Button",
  "ButtonGroup",
  "Card",
  "Checkbox",
  "Chip",
  "CollapsibleLabel",
  "Expand",
  "Expandable",
  "FlushSelect",
  "FormField",
  "HelpText",
  "IconButton",
  "IconText",
  "Input",
  "ListItem",
  "Markdown",
  "Modal",
  "OptionCard",
  "Popover",
  "SearchInput",
  "SectionHeader",
  "SubsectionHeader",
  "Select",
  "SidebarItem",
  "Slider",
  "Tabs",
  "Textarea",
  "Toggle",
] as const;

export type GalleryViewName = (typeof GALLERY_VIEWS)[number];
export type GalleryPrimitiveName = (typeof GALLERY_PRIMITIVES)[number];
