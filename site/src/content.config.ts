import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

const updates = defineCollection({
  loader: glob({ base: './src/content/updates', pattern: '**/*.mdx' }),
  schema: z.object({
    title: z.string(),
    date: z.coerce.date(),
    summary: z.string(),
  }),
});

const help = defineCollection({
  loader: glob({ base: './src/content/help', pattern: '**/*.mdx' }),
  schema: z.object({ title: z.string(), order: z.number() }),
});

const benchmarks = defineCollection({
  loader: glob({ base: './src/content/benchmarks', pattern: '**/*.mdx' }),
  schema: z.object({
    title: z.string(),
    date: z.coerce.date(),
    environment: z.string(),
    compiler: z.string(),
    warmupRuns: z.number(),
    measuredRuns: z.number(),
  }),
});

export const collections = { updates, help, benchmarks };
