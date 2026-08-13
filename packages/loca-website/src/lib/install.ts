// Download + how-to logic for the install page, shared by the no-JS baseline
// (InstallGenerator.astro) and the enhanced island (InstallGenerator.svelte) so
// the two never drift. Every download is a native installer published under
// version-less `current/<triple>/<stable-name>` R2 keys: the unified installer
// (Client + optional Core) on macOS/Windows, the Client AppImage + the Core
// .deb/.rpm on Linux (the one split platform), and the APK on Android.

export const STORAGE_BASE = "https://get.loca.rzkyif.com";

export type Os = "macos" | "linux" | "windows" | "android";
export type Channel = "stable" | "latest";

export interface OsChoice {
  id: Os;
  label: string;
  icon: string;
}

export const OS_CHOICES: OsChoice[] = [
  { id: "macos", label: "macOS", icon: "i-mdi-apple" },
  { id: "linux", label: "Linux", icon: "i-mdi-linux" },
  { id: "windows", label: "Windows", icon: "i-mdi-microsoft-windows" },
  { id: "android", label: "Android", icon: "i-mdi-android" },
];

// ---------------------------------------------------------------------------
// Native installer downloads
//
// Published by scripts/release/{unified-installers,core-installers,client}.ts.
// Each release mirrors every installer to a version-less
// `current/<triple>/<stable-name>` R2 key (exactly like {@link androidApkUrl}),
// so this page links a stable URL without knowing the version. The stable names
// here are kept in sync with installerAliasName() / clientBundleAlias() on the
// release side.

export type Arch = "arm64" | "x64";
export type InstallerFormat = "pkg" | "exe" | "deb" | "rpm" | "appimage";

export interface NativeInstaller {
  os: Os;
  arch: Arch;
  format: InstallerFormat;
  /** The direct download URL (the version-less `current/` alias). */
  url: string;
  /** Human arch label for the button, e.g. "Apple Silicon" / "Intel". */
  archLabel: string;
}

// [arch, target triple, human arch label] per desktop OS. Order is the one the
// page offers the buttons in (the most common arch first).
const OS_TRIPLES: Record<Exclude<Os, "android">, Array<[Arch, string, string]>> = {
  macos: [
    ["arm64", "aarch64-apple-darwin", "Apple Silicon"],
    ["x64", "x86_64-apple-darwin", "Intel"],
  ],
  // Only x86-64 Linux is built (RELEASE_TARGET_TRIPLES omits linux-arm64 as niche
  // for a desktop GUI); re-add the arm64 row here once that triple ships, or its
  // buttons point at artifacts that do not exist.
  linux: [["x64", "x86_64-unknown-linux-gnu", "Intel or AMD (x64)"]],
  windows: [
    ["x64", "x86_64-pc-windows-msvc", "Intel or AMD (x64)"],
    ["arm64", "aarch64-pc-windows-msvc", "ARM (ARM64)"],
  ],
};

// Channel binary suffix, matching channelBinSuffix() on the release side: stable
// is bare, other channels append `-<channel>`.
function binSuffix(channel: Channel): string {
  return channel === "stable" ? "" : `-${channel}`;
}

function currentUrl(channel: Channel, triple: string, filename: string): string {
  const prefix = channel === "stable" ? "" : `${channel}/`;
  return `${STORAGE_BASE}/${prefix}current/${triple}/${filename}`;
}

/** The MAIN download per desktop OS: the unified installer (macOS .pkg /
 *  Windows .exe, both installing the Client and optionally the Core) or the
 *  Linux Client AppImage. One entry per arch. Android has no installer here
 *  (it is a sideloaded APK, see {@link androidApkUrl}), so it returns []. */
export function appInstallers(os: Os, channel: Channel): NativeInstaller[] {
  if (os === "android") return [];
  const suffix = binSuffix(channel);
  const format: InstallerFormat = os === "macos" ? "pkg" : os === "windows" ? "exe" : "appimage";
  const filename =
    format === "pkg"
      ? `loca${suffix}.pkg`
      : format === "exe"
        ? `loca${suffix}-setup.exe`
        : // Tauri's capitalized ".AppImage" extension is part of the R2 key.
          `loca${suffix}.AppImage`;
  return OS_TRIPLES[os].map(([arch, triple, archLabel]) => ({
    os,
    arch,
    format,
    archLabel,
    url: currentUrl(channel, triple, filename),
  }));
}

/** The standalone Linux Core packages (.deb/.rpm), the only platform where the
 *  Core is a separate download (macOS/Windows fold it into the unified
 *  installer). One entry per format on the single Linux arch. */
export function linuxCoreInstallers(channel: Channel): NativeInstaller[] {
  const suffix = binSuffix(channel);
  const [arch, triple, archLabel] = OS_TRIPLES.linux[0];
  return (["deb", "rpm"] as const).map((format) => ({
    os: "linux",
    arch,
    format,
    archLabel,
    url: currentUrl(channel, triple, `loca-core${suffix}.${format}`),
  }));
}

/** The fixed "newest build" APK alias published per channel by
 *  scripts/release/android.ts, so the page can link a stable URL without
 *  knowing the version. The `current` segment is the moving alias; the channel
 *  prefix namespaces it (stable has none, latest is `latest/`). */
export function androidApkUrl(channel: Channel): string {
  const prefix = channel === "stable" ? "" : `${channel}/`;
  return `${STORAGE_BASE}/${prefix}current/android-universal/loca.apk`;
}

// ---------------------------------------------------------------------------
// How-to steps

/** A how-to step. Usually a plain line; a step can carry a lead line plus its
 *  own sub-points (the macOS Gatekeeper clear-it walkthrough, the AppImage
 *  run-it walkthrough), which the generators render as a nested list rather
 *  than one long sentence. */
export type HowtoStep = string | SubbedStep;

export interface SubbedStep {
  /** The lead line. */
  text: string;
  /** Sub-points, or [] for a one-line note. */
  sub: string[];
}

/** The per-OS steps for the downloaded installer. macOS/Windows run the
 *  unified installer (open it, choose whether the Core comes along, clear the
 *  first-run security prompt when it appears); the Linux Client is an AppImage
 *  you run in place. The shared finishing step is {@link finishStep}. */
export function installerHowto(os: Os): HowtoStep[] {
  if (os === "linux") {
    return [
      {
        text: "The AppImage is one portable file that keeps itself up to date. To run it:",
        sub: [
          "Make it executable, either from your file manager's properties (allow running it as a program) or with chmod +x on the file.",
          "Open it from your file manager or run it in a terminal. It runs in place, with no system install.",
        ],
      },
    ];
  }
  const steps: HowtoStep[] = [
    "Open the downloaded installer.",
    "Keep Loca Core selected to run your models and tools on this computer, and pick its options. Uncheck it only if this device should connect to a Core on another machine.",
    "Follow the prompts to finish the install.",
  ];
  const security = installerSecurityStep(os);
  if (security) steps.push(security);
  return steps;
}

/** The first-run security prompt an unsigned build triggers, per OS, or null
 *  when there is none. Phrased conditionally: signed builds are rolling out,
 *  and a signed installer never shows these. */
export function installerSecurityStep(os: Os): SubbedStep | null {
  if (os === "macos") {
    return {
      text: "If macOS blocks the installer as being from an unidentified developer (signed builds are rolling out), clear it once:",
      sub: [
        "Open System Settings, then Privacy & Security.",
        "Scroll to the Security section and choose Open Anyway next to the Loca message.",
        "Enter your password to confirm, then open the installer again.",
      ],
    };
  }
  if (os === "windows") {
    return {
      text: "If Windows warns that the publisher is unknown, choose More info, then Run anyway.",
      sub: [],
    };
  }
  return null;
}

/** The shared finishing step: what happens on first launch, per install shape. */
export function finishStep(os: Os): string {
  if (os === "linux") {
    return "Open Loca. It asks you to connect to a Core: one from the packages below on this machine, or one on another computer.";
  }
  return "Open Loca. If you installed the Core with it, Loca connects to it on its own; otherwise it asks you to connect to a Core on another computer.";
}

/** The Linux Core package steps, shown with the .deb/.rpm buttons. */
export function linuxCoreSteps(channel: Channel): string[] {
  return [
    `Install the package with your distribution's package manager (e.g. sudo apt install ./loca-core${binSuffix(channel)}.deb).`,
    "It registers the Core as a background service for your user; Loca connects to it on the next launch.",
  ];
}

/** The compact format tag a multi-format download button appends to its arch
 *  label (the Linux Core offers the .deb/.rpm packages). AppImage shows as a
 *  name, not a dotted extension; the packages keep their leading dot. */
export function formatTag(format: InstallerFormat): string {
  return format === "appimage" ? "AppImage" : `.${format}`;
}

// ---------------------------------------------------------------------------
// OS / arch detection

/** Best-effort map of a browser's CPU-architecture signal to one of our
 *  {@link Arch} ids, or null when it can't be told. The reliable source is the
 *  Chromium-only `navigator.userAgentData` high-entropy `architecture` value
 *  (passed in as `architecture`); the UA string is a coarse fallback for other
 *  browsers. macOS is deliberately unknowable from the UA (it always reads
 *  "Intel" even on Apple Silicon), so a Mac with no userAgentData stays null and
 *  the page highlights neither arch. We only ship 64-bit builds, so an "arm"
 *  signal maps to arm64 and an "x86" signal to x64. */
export function detectArch(architecture: string | undefined, ua: string): Arch | null {
  const a = (architecture ?? "").toLowerCase();
  if (a.includes("arm")) return "arm64";
  if (a.includes("x86") || a === "amd64") return "x64";
  const s = ua.toLowerCase();
  if (s.includes("aarch64") || s.includes("arm64")) return "arm64";
  // "win64"/"wow64"/"x86_64"/"x64" mark a 64-bit x86 browser; the bare macOS UA
  // carries none of these (its "Intel" token is intentionally ignored), so it
  // falls through to null.
  if (s.includes("win64") || s.includes("wow64") || s.includes("x86_64") || s.includes("x64")) {
    return "x64";
  }
  return null;
}

/** Map a navigator string to one of our OS ids, or null when none matches (an
 *  unrecognized/future platform). The caller picks a default OS for the picker;
 *  null is kept distinct so an unknown OS can fall back to arch-only highlighting
 *  rather than masquerading as macOS. */
export function detectOs(ua: string, platform: string): Os | null {
  const s = `${ua} ${platform}`.toLowerCase();
  if (s.includes("android")) return "android";
  if (s.includes("win")) return "windows";
  if (s.includes("linux")) return "linux";
  // mac, iphone, ipad all map to the macOS installer here.
  if (s.includes("mac") || s.includes("iphone") || s.includes("ipad")) {
    return "macos";
  }
  return null;
}
