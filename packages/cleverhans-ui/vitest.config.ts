import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    // @testing-library/react auto-cleanup hooks into the global afterEach.
    globals: true,
    setupFiles: ["./vitest.setup.ts"],
  },
});
