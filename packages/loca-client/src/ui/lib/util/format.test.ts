// byte formatter. Pure helper, no DOM.

import { describe, expect, it } from "vitest";
import { formatBytes } from "./format";

describe("formatBytes", () => {
  it("handles null and undefined as 'size unknown'", () => {
    expect(formatBytes(null)).toBe("size unknown");
    expect(formatBytes(undefined)).toBe("size unknown");
  });

  it("returns bytes for values under 1 KiB", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("returns KB at the KiB threshold and above", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1024 * 100)).toBe("100.0 KB");
  });

  it("returns MB at the MiB threshold and above", () => {
    expect(formatBytes(1024 ** 2)).toBe("1.0 MB");
    expect(formatBytes(1024 ** 2 * 250)).toBe("250.0 MB");
  });

  it("returns GB with two decimals at the GiB threshold and above", () => {
    expect(formatBytes(1024 ** 3)).toBe("1.00 GB");
    expect(formatBytes(1024 ** 3 * 2.5)).toBe("2.50 GB");
  });
});
