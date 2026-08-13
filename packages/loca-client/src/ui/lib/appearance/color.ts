/**
 * The color system lives in `@loca/shared/ui/color` so the client and the
 * website compute bubble/shadow/picker colors identically (single source - see
 * the single-source UI rule in AGENTS.md). This module re-exports it under the
 * client's `$lib/appearance/color` path so existing imports keep working.
 */
export * from "@loca/shared/ui/color";
