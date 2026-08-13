// Page object for the new-core / pairing wizard (used by the fresh-install
// pairing spec, which drives the real UI rather than the harness shortcut).

import { page } from "vitest/browser";
import { expect } from "vitest";

export class PairingPage {
  async expectVisible(): Promise<void> {
    await expect.element(page.getByTestId("mode-newCore")).toBeVisible({ timeout: 15_000 });
  }

  async fillRemote(url: string, code: string): Promise<void> {
    await page.getByTestId("pairing-url").fill(url);
    await page.getByTestId("pairing-code").fill(code);
  }

  async submit(): Promise<void> {
    await page.getByTestId("pairing-submit").click();
  }
}
