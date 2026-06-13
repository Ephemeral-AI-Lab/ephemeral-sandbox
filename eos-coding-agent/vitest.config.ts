import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    passWithNoTests: true,
    include: ["src/**/*.test.ts", "packages/**/*.test.ts"],
    exclude: ["**/node_modules/**", "**/legacy/**", "**/legacy-tests/**"],
  },
});
