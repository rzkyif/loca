// Sample prop bundles for every shared primitive (A0), consumed by the website
// gallery so each primitive gets a dedicated card showing its variant/state
// matrix. Keyed by the exact primitive component name (`Button`, `Alert`, ...)
// so the check-primitive-coverage walker can assert every primitives/*.svelte
// has samples here.
//
// A bundle carries only DATA props (the ones that define the variant being
// shown), including required data props. Callbacks and snippet/children props
// cannot live in a `.ts` file, so the gallery renderer supplies them per
// primitive. Each bundle is typed `Partial<ComponentProps<typeof X>>`: a wrong
// or misspelled prop name still fails svelte-check, while a required callback /
// snippet the renderer injects may be omitted.

import type { Component, ComponentProps, SvelteComponent } from "svelte";
import type ActionSheet from "../components/primitives/ActionSheet.svelte";
import type Alert from "../components/primitives/Alert.svelte";
import type Bubble from "../components/primitives/Bubble.svelte";
import type Button from "../components/primitives/Button.svelte";
import type ButtonGroup from "../components/primitives/ButtonGroup.svelte";
import type Card from "../components/primitives/Card.svelte";
import type Checkbox from "../components/primitives/Checkbox.svelte";
import type Chip from "../components/primitives/Chip.svelte";
import type CollapsibleLabel from "../components/primitives/CollapsibleLabel.svelte";
import type Expand from "../components/primitives/Expand.svelte";
import type Expandable from "../components/primitives/Expandable.svelte";
import type FlushSelect from "../components/primitives/FlushSelect.svelte";
import type FormField from "../components/primitives/FormField.svelte";
import type HelpText from "../components/primitives/HelpText.svelte";
import type IconButton from "../components/primitives/IconButton.svelte";
import type IconText from "../components/primitives/IconText.svelte";
import type Input from "../components/primitives/Input.svelte";
import type ListItem from "../components/primitives/ListItem.svelte";
import type Markdown from "../components/primitives/Markdown.svelte";
import type Modal from "../components/primitives/Modal.svelte";
import type OptionCard from "../components/primitives/OptionCard.svelte";
import type Popover from "../components/primitives/Popover.svelte";
import type SearchInput from "../components/primitives/SearchInput.svelte";
import type SectionHeader from "../components/primitives/SectionHeader.svelte";
import type SubsectionHeader from "../components/primitives/SubsectionHeader.svelte";
import type Select from "../components/primitives/Select.svelte";
import type SidebarItem from "../components/primitives/SidebarItem.svelte";
import type Slider from "../components/primitives/Slider.svelte";
import type Tabs from "../components/primitives/Tabs.svelte";
import type Textarea from "../components/primitives/Textarea.svelte";
import type Toggle from "../components/primitives/Toggle.svelte";

type Bundle<C extends Component<any, any> | SvelteComponent> = Record<
  string,
  Partial<ComponentProps<C>>
>;

const SELECT_OPTIONS = [
  { value: "fast", label: "Fast" },
  { value: "balanced", label: "Balanced" },
  { value: "thorough", label: "Thorough" },
];

const actionSheetSamples = {
  default: { open: true, title: "Message" },
} satisfies Bundle<typeof ActionSheet>;

const alertSamples = {
  info: { variant: "info" },
  success: { variant: "success" },
  warning: { variant: "warning" },
  error: { variant: "error" },
  transparent: { variant: "info", surface: "transparent" },
  small: { variant: "warning", size: "sm" },
  startAligned: { variant: "error", align: "start" },
} satisfies Bundle<typeof Alert>;

const bubbleSamples = {
  left: { selectedAlignment: "left" },
  right: { selectedAlignment: "right" },
  center: { selectedAlignment: "center" },
  small: { selectedAlignment: "left", size: "small" },
  accent: { selectedAlignment: "left", accent: "blue" },
  active: { selectedAlignment: "left", active: true },
  progress: { selectedAlignment: "left", progress: 60 },
  indeterminate: { selectedAlignment: "left", progress: null },
} satisfies Bundle<typeof Bubble>;

const buttonSamples = {
  primary: { variant: "primary" },
  secondary: { variant: "secondary" },
  destructive: { variant: "destructive" },
  ghost: { variant: "ghost" },
  withIcon: { variant: "primary", icon: "i-material-symbols-add-rounded" },
  loading: { variant: "primary", loading: true },
  disabled: { variant: "primary", disabled: true },
  small: { variant: "secondary", size: "sm" },
} satisfies Bundle<typeof Button>;

const buttonGroupSamples = {
  row: { direction: "row" },
  column: { direction: "column" },
  transparent: { surface: "transparent" },
} satisfies Bundle<typeof ButtonGroup>;

const cardSamples = {
  default: { variant: "default" },
  raised: { variant: "raised" },
  paddingLg: { padding: "lg" },
  paddingNone: { padding: "none" },
} satisfies Bundle<typeof Card>;

const checkboxSamples = {
  unchecked: { checked: false },
  checked: { checked: true },
  indeterminate: { indeterminate: true },
  disabled: { checked: true, disabled: true },
} satisfies Bundle<typeof Checkbox>;

const chipSamples = {
  default: { label: "default", variant: "default" },
  subtle: { label: "subtle", variant: "subtle" },
  accent: { label: "accent", variant: "accent", accent: "blue" },
  withIcon: { label: "tagged", icon: "i-material-symbols-label-outline-rounded" },
  green: { label: "green", variant: "accent", accent: "green" },
  red: { label: "red", variant: "accent", accent: "red" },
  small: { label: "small", size: "sm" },
} satisfies Bundle<typeof Chip>;

const collapsibleLabelSamples = {
  expanded: { collapsed: false },
  collapsed: { collapsed: true },
} satisfies Bundle<typeof CollapsibleLabel>;

const expandSamples = {
  open: { open: true },
  closed: { open: false },
} satisfies Bundle<typeof Expand>;

const expandableSamples = {
  collapsed: { expanded: false },
  expanded: { expanded: true },
  disabled: { disabled: true },
} satisfies Bundle<typeof Expandable>;

const flushSelectSamples = {
  default: { value: "balanced", options: SELECT_OPTIONS, ariaLabel: "Mode" },
  withIcon: {
    value: "fast",
    options: SELECT_OPTIONS,
    ariaLabel: "Mode",
    icon: "i-material-symbols-tune-rounded",
  },
  disabled: { value: "fast", options: SELECT_OPTIONS, ariaLabel: "Mode", disabled: true },
} satisfies Bundle<typeof FlushSelect>;

const formFieldSamples = {
  vertical: { label: "Display name" },
  withDescription: { label: "Display name", description: "Shown on every message you send." },
  horizontal: { label: "Compact mode", horizontal: true },
  withError: { label: "Port", error: "Must be between 1 and 65535" },
} satisfies Bundle<typeof FormField>;

const helpTextSamples = {
  default: {
    text: "This setting controls how the assistant formats its replies.",
    variant: "default",
  },
  compact: { text: "Applies on the next message.", variant: "compact" },
} satisfies Bundle<typeof HelpText>;

const iconButtonSamples = {
  default: {
    icon: "i-material-symbols-settings-outline-rounded",
    title: "Settings",
    variant: "default",
  },
  subtle: { icon: "i-material-symbols-close-rounded", title: "Close", variant: "subtle" },
  filled: { icon: "i-material-symbols-add-rounded", title: "Add", surface: "filled" },
  circle: { icon: "i-material-symbols-mic-outline-rounded", title: "Record", surface: "circle" },
  active: { icon: "i-material-symbols-star-rounded", title: "Star", active: true },
  disabled: { icon: "i-material-symbols-delete-outline-rounded", title: "Delete", disabled: true },
  large: { icon: "i-material-symbols-send-rounded", title: "Send", size: "lg" },
} satisfies Bundle<typeof IconButton>;

const iconTextSamples = {
  status: { icon: "i-material-symbols-check-circle-rounded", color: "text-default-700" },
  error: { icon: "i-material-symbols-error-rounded", color: "text-accent-red-700" },
  header: {
    icon: "i-material-symbols-shield-question-rounded",
    color: "text-default-800",
  },
} satisfies Bundle<typeof IconText>;

const inputSamples = {
  default: { value: "Hello" },
  placeholder: { value: "", placeholder: "Type something..." },
  number: { type: "number", value: 8080, spinner: true },
  password: { type: "password", value: "secret" },
  error: { value: "nope", error: true },
  disabled: { value: "locked", disabled: true },
  withSuffix: { value: "60", suffix: "seconds" },
  mono: { value: "~/.loca/stable", mono: true },
} satisfies Bundle<typeof Input>;

const listItemSamples = {
  default: { selected: false },
  selected: { selected: true },
  row: { direction: "row" },
  disabled: { disabled: true },
} satisfies Bundle<typeof ListItem>;

const markdownSamples = {
  default: {
    content:
      "## Heading\n\nA paragraph with **bold**, *italic*, and `inline code`.\n\n- one\n- two",
  },
  streaming: { content: "Generating a reply", isStreaming: true },
} satisfies Bundle<typeof Markdown>;

const modalSamples = {
  default: { open: true, positioning: "absolute", maxWidth: "sm" },
} satisfies Bundle<typeof Modal>;

const optionCardSamples = {
  unselected: { selected: false, title: "Balanced", description: "A good default." },
  selected: { selected: true, title: "Balanced", description: "A good default." },
  accent: { selected: true, selectedStyle: "accent", accent: "green", title: "Thorough" },
  withIcon: { selected: false, icon: "i-material-symbols-bolt-rounded", title: "Fast" },
  small: { selected: false, size: "sm", title: "Compact" },
} satisfies Bundle<typeof OptionCard>;

const popoverSamples = {
  default: { open: true, placement: "bottom" },
} satisfies Bundle<typeof Popover>;

const searchInputSamples = {
  empty: { value: "" },
  withValue: { value: "memory" },
  disabled: { value: "", disabled: true },
} satisfies Bundle<typeof SearchInput>;

const sectionHeaderSamples = {
  section: { label: "Appearance", level: "section" },
  group: { label: "General", level: "group" },
  collapsibleCollapsed: { label: "Advanced", collapsible: true, expanded: false },
  collapsibleExpanded: { label: "Advanced", collapsible: true, expanded: true },
} satisfies Bundle<typeof SectionHeader>;

const subsectionHeaderSamples = {
  default: { label: "Bundled files" },
  withActions: { label: "Permissions" },
} satisfies Bundle<typeof SubsectionHeader>;

const selectSamples = {
  default: { value: "balanced", options: SELECT_OPTIONS },
  disabled: { value: "fast", options: SELECT_OPTIONS, disabled: true },
  error: { value: "balanced", options: SELECT_OPTIONS, error: true },
  invisible: { value: "balanced", options: SELECT_OPTIONS, variant: "invisible" },
} satisfies Bundle<typeof Select>;

const sidebarItemSamples = {
  default: { icon: "i-material-symbols-chat-outline-rounded", label: "Chat", collapsed: false },
  selected: {
    icon: "i-material-symbols-settings-outline-rounded",
    label: "Settings",
    collapsed: false,
    selected: true,
  },
  collapsed: { icon: "i-material-symbols-chat-outline-rounded", label: "Chat", collapsed: true },
  ping: {
    icon: "i-material-symbols-download-rounded",
    label: "Updates",
    collapsed: false,
    ping: true,
  },
  pingAccent: {
    icon: "i-material-symbols-download-rounded",
    label: "Updates",
    collapsed: false,
    ping: true,
    pingTone: "accent",
  },
  disabled: {
    icon: "i-material-symbols-lock-outline",
    label: "Locked",
    collapsed: false,
    disabled: true,
  },
} satisfies Bundle<typeof SidebarItem>;

const sliderSamples = {
  default: { value: 40, min: 0, max: 100 },
  paired: { value: 40, min: 0, max: 100, pairedInput: true },
  withSuffix: { value: 12, min: 1, max: 32, suffix: "px" },
  disabled: { value: 40, min: 0, max: 100, disabled: true },
  error: { value: 40, min: 0, max: 100, pairedInput: true, error: true },
} satisfies Bundle<typeof Slider>;

const tabsSamples = {
  default: {
    tabs: [
      { id: "a", label: "General" },
      { id: "b", label: "Appearance" },
      { id: "c", label: "Advanced" },
    ],
    active: "b",
  },
} satisfies Bundle<typeof Tabs>;

const textareaSamples = {
  default: { value: "A few lines of\nplaceholder content." },
  placeholder: { value: "", placeholder: "Write a memory..." },
  error: { value: "invalid", error: true },
  disabled: { value: "read only", disabled: true },
  mono: { value: "const x = 1", mono: true },
} satisfies Bundle<typeof Textarea>;

const toggleSamples = {
  off: { checked: false },
  on: { checked: true },
  multiChoice: {
    options: [
      { value: "off", label: "Off" },
      { value: "auto", label: "Auto" },
      { value: "on", label: "On" },
    ],
    value: "auto",
  },
  disabled: { checked: true, disabled: true },
  multiChoiceDisabled: {
    options: [
      { value: "off", label: "Off" },
      { value: "auto", label: "Auto" },
      { value: "on", label: "On" },
    ],
    value: "auto",
    disabled: true,
  },
} satisfies Bundle<typeof Toggle>;

/** Primitive component name -> its named sample scenarios (data props only). */
export const PRIMITIVE_SAMPLES = {
  ActionSheet: actionSheetSamples,
  Alert: alertSamples,
  Bubble: bubbleSamples,
  Button: buttonSamples,
  ButtonGroup: buttonGroupSamples,
  Card: cardSamples,
  Checkbox: checkboxSamples,
  Chip: chipSamples,
  CollapsibleLabel: collapsibleLabelSamples,
  Expand: expandSamples,
  Expandable: expandableSamples,
  FlushSelect: flushSelectSamples,
  FormField: formFieldSamples,
  HelpText: helpTextSamples,
  IconButton: iconButtonSamples,
  IconText: iconTextSamples,
  Input: inputSamples,
  ListItem: listItemSamples,
  Markdown: markdownSamples,
  Modal: modalSamples,
  OptionCard: optionCardSamples,
  Popover: popoverSamples,
  SearchInput: searchInputSamples,
  SectionHeader: sectionHeaderSamples,
  SubsectionHeader: subsectionHeaderSamples,
  Select: selectSamples,
  SidebarItem: sidebarItemSamples,
  Slider: sliderSamples,
  Tabs: tabsSamples,
  Textarea: textareaSamples,
  Toggle: toggleSamples,
} as const;
