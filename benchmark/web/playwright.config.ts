import { defineConfig, devices } from "@playwright/test";

const evidenceRoot = process.env.BENCHMARK_EVIDENCE_ROOT;

export default defineConfig({
  testDir: "./tests/browser",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  timeout: process.env.BENCHMARK_REAL_BACKEND_URL ? 30 * 60_000 : 30_000,
  outputDir: evidenceRoot ? `${evidenceRoot}/playwright-results` : "./test-results",
  reporter: evidenceRoot
    ? [["list"], ["json", { outputFile: `${evidenceRoot}/playwright-report.json` }], ["html", { outputFolder: `${evidenceRoot}/playwright-html`, open: "never" }]]
    : [["list"], ["html", { open: "never" }]],
  expect: {
    toHaveScreenshot: { animations: "disabled", caret: "hide", maxDiffPixelRatio: 0.01 },
  },
  use: {
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    contextOptions: { reducedMotion: "reduce" },
  },
  projects: [
    {
      name: "fixture",
      testMatch: /fixture\/.*\.spec\.ts/,
      use: { ...devices["Desktop Chrome"], baseURL: "http://127.0.0.1:4173", colorScheme: "light" },
    },
    {
      name: "real-backend",
      testMatch: /real-backend\/.*\.spec\.ts/,
      fullyParallel: false,
      workers: 1,
      use: { ...devices["Desktop Chrome"], baseURL: process.env.BENCHMARK_REAL_BACKEND_URL, colorScheme: "light" },
    },
  ],
  webServer: process.env.BENCHMARK_REAL_BACKEND_URL
    ? undefined
    : {
        command: "npm run dev -- --host 127.0.0.1 --port 4173 --strictPort",
        url: "http://127.0.0.1:4173/benchmark",
        reuseExistingServer: !process.env.CI,
      },
});
