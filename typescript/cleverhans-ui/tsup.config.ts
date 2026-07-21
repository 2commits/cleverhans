import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm"],
  dts: true,
  sourcemap: true,
  outDir: "dist",
  // Interactive chat widgets — client-only by nature. The directive makes
  // that explicit for Next.js App Router consumers.
  banner: { js: '"use client";' },
});
