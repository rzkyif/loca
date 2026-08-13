/**
 * The composer's attachment pipeline: the pending attachment list plus every
 * way one gets added (file picker, mobile image/doc pickers, clipboard paste,
 * OS drag-and-drop, screen / region capture) and the image-preview modal state.
 *
 * Per the composable convention (see use-autocomplete), this class holds only
 * the `$state` and the imperative handlers; the consumer keeps the `$derived`
 * views (attachmentParts) and lifecycle (onMount capture-monitor fetch, the
 * input-shortcut wiring) and supplies its hooks (focus, onMobile, notice) via
 * the constructor context. It never reaches into the send/streaming path.
 */

import type { Attachment } from "$lib/util/types";
import { platform } from "$lib/platform";
import { getLogger } from "$lib/util/log";
import { settingsState } from "$stores/settings.svelte";
import {
  classifyAttachment,
  DOC_EXTENSIONS,
  IMAGE_EXTENSIONS,
  ingestDocumentBlob,
  ingestDocumentFromPath,
  ingestImageBlob,
  ingestTextDocument,
  MIME_BY_EXT,
  TEXT_DOC_EXTENSIONS,
} from "$lib/chat/attachments";
import { pauseClickThrough, resumeClickThrough } from "$lib/window/window";
import {
  captureMonitor,
  type CaptureMonitorInfo,
  captureRegion,
  listCaptureMonitors,
} from "$lib/capture/capture";

const attachLog = getLogger("attach");

export type ComposerAttachmentsContext = {
  // True on the Android client: pickers and the transparent-window dance differ.
  onMobile: boolean;
  // Refocus the textarea after an attachment lands (desktop only; the consumer
  // suppresses it on mobile to keep the soft keyboard down).
  focus: () => void;
};

export class ComposerAttachments {
  attachments = $state<Attachment[]>([]);
  previewImageUrl = $state<string | null>(null);
  capturing = $state(false);
  captureMonitors = $state<CaptureMonitorInfo[]>([]);

  constructor(private readonly ctx: ComposerAttachmentsContext) {}

  /** Populate the monitor list for the always-visible screen-capture select.
   *  Called from the consumer's onMount; best-effort. */
  async loadCaptureMonitors(): Promise<void> {
    this.captureMonitors = await listCaptureMonitors();
  }

  clear(): void {
    this.attachments = [];
  }

  removeAttachment(index: number): void {
    this.attachments = this.attachments.filter((_, i) => i !== index);
  }

  openImagePreview(url: string): void {
    if (url) this.previewImageUrl = url;
  }

  closeImagePreview(): void {
    this.previewImageUrl = null;
  }

  /** Classify and ingest a batch of in-memory files (from a paste or an OS
   *  drop), appending each supported one to the pending list. Unsupported
   *  files are skipped. Returns whether any file was accepted. Images ingest
   *  straight from their bytes; documents go through the binary converter. */
  private async ingestFiles(files: File[]): Promise<boolean> {
    if (files.length === 0) return false;

    const supportsImages = !!settingsState.currentSettings["llm.supportImages"];
    let handledAny = false;

    for (const file of files) {
      const name = file.name || "pasted";
      const kind = classifyAttachment(name, file.type, supportsImages);
      if (!kind) continue;

      handledAny = true;
      try {
        if (kind.kind === "image") {
          const baseName = name.includes(".") ? name : `${name}.${kind.ext}`;
          this.attachments = [...this.attachments, await ingestImageBlob(file, baseName, kind.ext)];
        } else {
          this.attachments = [...this.attachments, await ingestDocumentBlob(file, name)];
        }
      } catch (err) {
        attachLog.error("Attachment ingestion failed:", err);
      }
    }

    return handledAny;
  }

  async handlePaste(e: ClipboardEvent): Promise<void> {
    const files = filesFromDataTransfer(e.clipboardData);
    if (files.length === 0) return;
    // Read the files synchronously above (a DataTransfer is emptied once the
    // event handler returns), then ingest.
    if (await this.ingestFiles(files)) e.preventDefault();
  }

  // OS drag-and-drop. `dragDropEnabled` is false on the desktop window (see
  // tauri.conf.json), so the webview delivers native HTML5 drop events carrying
  // real File objects, which flow through the same classify + ingest path as
  // paste; the dropped file just appears in the pending list (no overlay). The
  // app-wide dragover guard in +layout.svelte cancels file dragover, which is
  // what makes the window a valid drop target, so no dragover handler is needed
  // here. The caller gates this on compose mode (a prompt mode doesn't accept
  // attachments).
  //
  // Click-through coexistence: the overlay-window's poll (lib/window/window.ts)
  // tracks the OS cursor independently of DOM events and disables click-through
  // while the cursor is over the opaque composer, so a drop there still reaches
  // the webview; over the transparent gaps it stays click-through as before.
  // Mobile has no OS file drag, so this never fires there.
  async handleDrop(e: DragEvent): Promise<void> {
    if (!dragHasFiles(e)) return;
    e.preventDefault();
    const files = e.dataTransfer ? Array.from(e.dataTransfer.files) : [];
    if (await this.ingestFiles(files)) this.ctx.focus();
  }

  // Android: the picker returns a content URI (not an absolute path), so picked
  // files are read as bytes. The OS offers no single files+photos picker, so the
  // composer shows two buttons backed by these two handlers. There is no
  // transparent-window dance to perform.
  //
  // Image picker: photos need no conversion, so any model-supported image is
  // ingested straight from its bytes.
  async handleAttachImageMobile(): Promise<void> {
    const supportsImages = settingsState.currentSettings["llm.supportImages"];
    if (!supportsImages) {
      attachLog.warn("this model has images disabled; the image picker is a no-op");
      return;
    }
    const picked = await platform().dialog.openFilePicker({
      multiple: false,
      filters: [{ name: "Images", extensions: IMAGE_EXTENSIONS }],
    });
    if (picked.length === 0) return;
    const uri = picked[0];
    const data = await platform().fs.readFile(uri);
    const rawName = decodeURIComponent(uri.split("/").pop() || "image");
    const ext = rawName.split(".").pop()?.toLowerCase() || "";
    const safeExt = IMAGE_EXTENSIONS.includes(ext) ? ext : "png";
    const fileName = rawName.includes(".") ? rawName : `image.${safeExt}`;
    const mime = MIME_BY_EXT[safeExt] || "image/png";
    // .slice() yields a fresh non-shared ArrayBuffer, which Blob's BlobPart needs.
    const blob = new Blob([data.slice().buffer], { type: mime });
    this.attachments = [...this.attachments, await ingestImageBlob(blob, fileName, safeExt)];
  }

  // Document picker: the binary converter (anytomd/pdf-extract) is desktop-only,
  // so on mobile only the plain-text document types (txt, md, code, csv, json,
  // ...) are offered; their bytes are decoded as UTF-8 and ingested directly. A
  // binary office/pdf file picked anyway is skipped with a notice.
  async handleAttachDocMobile(): Promise<void> {
    const picked = await platform().dialog.openFilePicker({
      multiple: false,
      filters: [{ name: "Documents", extensions: TEXT_DOC_EXTENSIONS }],
    });
    if (picked.length === 0) return;
    const uri = picked[0];
    const rawName = decodeURIComponent(uri.split("/").pop() || "document.txt");
    const ext = rawName.split(".").pop()?.toLowerCase() || "";
    if (!TEXT_DOC_EXTENSIONS.includes(ext)) {
      attachLog.warn(`cannot attach .${ext} on mobile: it needs the desktop document converter`);
      return;
    }
    const fileName = rawName.includes(".") ? rawName : `${rawName}.txt`;
    const data = await platform().fs.readFile(uri);
    const text = new TextDecoder().decode(data);
    this.attachments = [...this.attachments, ingestTextDocument(text, fileName)];
  }

  async handleAttachFile(): Promise<void> {
    try {
      const supportsImages = settingsState.currentSettings["llm.supportImages"];

      if (this.ctx.onMobile) {
        // The composer's "Attach File" button is the document picker on mobile;
        // images have their own button (handleAttachImageMobile).
        await this.handleAttachDocMobile();
        return;
      }

      const filters: Array<{ name: string; extensions: string[] }> = [];

      // With images enabled there are two categories, so lead with a combined
      // "All supported types" filter as the default selection. Without images
      // the "Documents" filter already covers everything supported.
      if (supportsImages) {
        filters.push({
          name: "All supported types",
          extensions: [...DOC_EXTENSIONS, ...IMAGE_EXTENSIONS],
        });
      }

      filters.push({
        name: "Documents",
        extensions: DOC_EXTENSIONS,
      });

      if (supportsImages) {
        filters.push({
          name: "Images",
          extensions: IMAGE_EXTENSIONS,
        });
      }

      // Shrink window and center Y to prevent macOS dimming overlay on
      // transparent window while the native file picker is open.
      const win = platform().windowing;
      const savedSize = await win.outerSize();
      const savedPos = await win.outerPosition();
      const monitor = await win.currentMonitor();
      const centerX = Math.round(savedPos.x + savedSize.width / 2);
      const centerY = monitor ? Math.round(monitor.y + monitor.height / 3) : savedPos.y;
      await pauseClickThrough();
      await win.setOuterSize({ width: 1, height: 1 });
      await win.setOuterPosition({ x: centerX, y: centerY });

      let picked: string[];
      try {
        picked = await platform().dialog.openFilePicker({
          multiple: false,
          filters,
        });
      } finally {
        await win.setOuterPosition({ x: savedPos.x, y: savedPos.y });
        await win.setOuterSize({
          width: savedSize.width,
          height: savedSize.height,
        });
        await resumeClickThrough();
      }

      if (picked.length === 0) return;
      const filePath = picked[0];
      const fileName = filePath.split("/").pop() || filePath.split("\\").pop() || "file";
      const ext = fileName.split(".").pop()?.toLowerCase() || "";

      if (IMAGE_EXTENSIONS.includes(ext)) {
        const data = await platform().fs.readFile(filePath);
        const mime = MIME_BY_EXT[ext] || "image/png";
        // .slice() returns a Uint8Array backed by a fresh non-shared
        // ArrayBuffer, which Blob's `BlobPart` signature requires.
        const blob = new Blob([data.slice().buffer], { type: mime });
        this.attachments = [...this.attachments, await ingestImageBlob(blob, fileName, ext)];
      } else {
        this.attachments = [...this.attachments, await ingestDocumentFromPath(filePath, fileName)];
      }
      this.ctx.focus();
    } catch (err) {
      attachLog.error("File attach failed:", err);
    }
  }

  handleCaptureSelect(e: Event): void {
    const target = e.target as HTMLSelectElement;
    const id = target.value;
    target.value = "";
    if (id) void this.captureMonitorById(id);
  }

  async captureMonitorById(monitorId: string): Promise<void> {
    if (this.capturing) return;
    // A screenshot is an image attachment; skip when the model can't read images
    // (the capture controls are hidden then, but a bound shortcut still reaches
    // here).
    if (!settingsState.currentSettings["llm.supportImages"]) {
      attachLog.warn("screen capture skipped: image support is off");
      return;
    }
    this.capturing = true;
    try {
      const base64 = await captureMonitor(monitorId);
      if (base64) {
        this.attachments = [
          ...this.attachments,
          {
            type: "image",
            filename: `screenshot-${Date.now()}.png`,
            pendingData: base64,
            mime: "image/png",
          },
        ];
        this.ctx.focus();
      }
    } finally {
      this.capturing = false;
    }
  }

  async handleCaptureRegionFromMenu(): Promise<void> {
    if (this.capturing) return;
    if (!settingsState.currentSettings["llm.supportImages"]) {
      attachLog.warn("region capture skipped: image support is off");
      return;
    }
    this.capturing = true;
    try {
      const base64 = await captureRegion();
      if (base64) {
        this.attachments = [
          ...this.attachments,
          {
            type: "image",
            filename: `region-${Date.now()}.png`,
            pendingData: base64,
            mime: "image/png",
          },
        ];
        this.ctx.focus();
      }
    } finally {
      this.capturing = false;
    }
  }
}

/** Whether a drag operation is carrying files (as opposed to dragged text or a
 *  URL), so the drag handlers only engage for a real file drop. */
function dragHasFiles(e: DragEvent): boolean {
  return Array.from(e.dataTransfer?.types ?? []).includes("Files");
}

/** Pull the files out of a DataTransfer (a paste's clipboardData or a drop).
 *  Prefers `.files`; when that is empty, some sources (notably macOS/Linux
 *  webviews pasting a file copied in the file manager) expose the file only as
 *  an item, so fall back to scanning `.items` for file entries. */
function filesFromDataTransfer(dt: DataTransfer | null): File[] {
  if (!dt) return [];
  if (dt.files.length > 0) return Array.from(dt.files);
  const out: File[] = [];
  for (const item of Array.from(dt.items)) {
    if (item.kind === "file") {
      const f = item.getAsFile();
      if (f) out.push(f);
    }
  }
  return out;
}
