<script lang="ts">
  import type { SettingField } from "@loca/shared";
  import { evalCondition } from "@loca/shared";
  import { settingsState } from "../../../state";
  import FieldCard from "./FieldCard.svelte";
  import ShortcutFieldView from "@loca/shared/ui/components/settings/ShortcutFieldView.svelte";

  let {
    field,
    error,
    horizontal = false,
    onChange,
    onReset,
  } = $props<{
    field: SettingField;
    error: string | null;
    horizontal?: boolean;
    onChange: (key: string, value: any) => void;
    onReset: (fieldId: string) => void;
  }>();

  const editable = $derived(evalCondition(field.editableWhen, settingsState.currentSettings));
  const currentValue = $derived((settingsState.currentSettings[field.id] ?? "") as string);
  const hasError = $derived(!!error);

  let capturing = $state(false);

  // Map a KeyboardEvent.code to a Tauri accelerator key segment.
  // Returns null for keys that should not commit (modifiers, Escape).
  function codeToAcceleratorKey(code: string): string | null {
    if (code.startsWith("Key")) return code.slice(3).toLowerCase();
    if (code.startsWith("Digit")) return code.slice(5);
    if (code.startsWith("Numpad")) {
      const rest = code.slice(6);
      if (/^\d$/.test(rest)) return `num${rest}`;
      return null;
    }
    if (/^F\d{1,2}$/.test(code)) return code.toLowerCase();
    if (code.startsWith("Arrow")) return code.slice(5).toLowerCase();
    switch (code) {
      case "Space":
        return "space";
      case "Enter":
        return "enter";
      case "Tab":
        return "tab";
      case "Backspace":
        return "backspace";
      case "Delete":
        return "delete";
      case "Insert":
        return "insert";
      case "Home":
        return "home";
      case "End":
        return "end";
      case "PageUp":
        return "pageup";
      case "PageDown":
        return "pagedown";
      case "Minus":
        return "-";
      case "Equal":
        return "=";
      case "BracketLeft":
        return "[";
      case "BracketRight":
        return "]";
      case "Backslash":
        return "\\";
      case "Semicolon":
        return ";";
      case "Quote":
        return "'";
      case "Comma":
        return ",";
      case "Period":
        return ".";
      case "Slash":
        return "/";
      case "Backquote":
        return "`";
      default:
        return null;
    }
  }

  function buildAccelerator(e: KeyboardEvent): string | null {
    const key = codeToAcceleratorKey(e.code);
    if (!key) return null;
    const parts: string[] = [];
    if (e.metaKey) parts.push("super");
    if (e.ctrlKey) parts.push("ctrl");
    if (e.altKey) parts.push("alt");
    if (e.shiftKey) parts.push("shift");
    parts.push(key);
    return parts.join("+");
  }

  function startCapture() {
    if (!editable) return;
    capturing = true;
  }

  function clearShortcut(e: MouseEvent) {
    e.stopPropagation();
    if (!editable) return;
    onChange(field.id, "");
    capturing = false;
  }

  // Capture key combos at the window level so we don't depend on the button
  // having keyboard focus (WebKit doesn't focus buttons on click by default,
  // which would otherwise drop the keydown silently).
  $effect(() => {
    if (!capturing) return;

    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();

      if (e.code === "Escape") {
        capturing = false;
        return;
      }

      const accel = buildAccelerator(e);
      if (!accel) return; // lone modifier, wait for a real key

      onChange(field.id, accel);
      capturing = false;
    }

    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, true);
  });

  const segments = $derived(currentValue ? currentValue.split("+") : []);
</script>

<FieldCard {field} {error} {horizontal} {onReset}>
  <ShortcutFieldView
    fieldName={field.name}
    {segments}
    recording={capturing}
    {editable}
    {hasError}
    showClear={editable && !!currentValue}
    onStartCapture={startCapture}
    onClear={clearShortcut}
  />
</FieldCard>
