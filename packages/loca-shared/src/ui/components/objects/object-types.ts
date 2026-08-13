/** Shared shapes for the object-management UI components (ObjectBadge,
 *  ObjectCard, ObjectDetailHeader) and the per-type settings fields that feed
 *  them. Deliberately tiny: just the badge value object. */

/** Accent hues available to badges (mirrors Chip.svelte's accents). */
export type Accent = "blue" | "green" | "red" | "yellow" | "purple";

/** A small status chip shown on a card or in a detail header. `accent` colors
 *  it (status semantics); omit for the neutral bg-surface-inset variant. */
export interface Badge {
  label: string;
  icon?: string;
  accent?: Accent;
  /** Tooltip (e.g. a extension's lastError). */
  title?: string;
}
