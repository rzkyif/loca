import type { Snippet } from "svelte";

// Snippet props are supplied by the gallery/showcase renderer at render time (a
// snippet cannot live in a `.ts` sample), so they are excluded from a sample
// bundle's type. Everything else - including REQUIRED data props - stays
// required, so a sample that drops a required prop fails svelte-check (the drift
// `Partial<ComponentProps<...>>` used to hide). Which props are snippets is read
// off each View's own prop types rather than a hand-maintained name list, so a
// prop like `badges`/`actions`/`title`/`field` that is a snippet on one
// component but a DATA prop on another is stripped only where it is actually a
// snippet, and never wrongly dropped from the samples that supply it as data.
type SnippetPropName<P> = {
  [K in keyof P]-?: NonNullable<P[K]> extends Snippet<any> ? K : never;
}[keyof P];

/** A View's props minus its snippet props: the shape a sample bundle provides. */
export type OmitSnippetProps<P> = Omit<P, SnippetPropName<P>>;
