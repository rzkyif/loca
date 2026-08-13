import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

// The user manual: one MDX file per subsection (each becomes its own page). Pages
// render shared @loca/shared/ui components inline as demos, placed at the point
// each system is discussed (see the root COPY.md). The "Last updated" date shown on each
// page is derived from the file's last git commit at build time (see
// src/lib/git-date.ts), not declared in frontmatter.
const manual = defineCollection({
  loader: glob({ pattern: "**/*.{md,mdx}", base: "./src/content/manual" }),
  schema: z.object({
    title: z.string(),
    section: z.string(),
    /** Sort order within the section. */
    order: z.number().default(0),
  }),
});

export const collections = { manual };
