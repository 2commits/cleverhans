import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm"],
  dts: true,
  sourcemap: true,
  outDir: "dist",
  // The package is hooks + a live transport — client-only by nature. The
  // directive makes that explicit for Next.js App Router consumers.
  banner: { js: '"use client";' },
});
