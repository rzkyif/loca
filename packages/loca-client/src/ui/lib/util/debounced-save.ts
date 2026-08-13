// A debounced save for the settings detail editors: keystrokes call
// `scheduleSave` to coalesce writes, blur (or an explicit action) calls
// `flushSave` to write immediately. Both clear the pending timer first, so a
// flush never races a scheduled write. The `save` callback owns its own guards
// (validation, dirty checks) and error handling.

export function createDebouncedSave(save: () => void | Promise<void>, delayMs = 500) {
  let timer: ReturnType<typeof setTimeout> | null = null;

  function cancel(): void {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function scheduleSave(): void {
    cancel();
    timer = setTimeout(() => void flushSave(), delayMs);
  }

  async function flushSave(): Promise<void> {
    cancel();
    await save();
  }

  return { scheduleSave, flushSave };
}
