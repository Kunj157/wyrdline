import { defineConfig, devices } from "@playwright/test";

/**
 * Studio against a real wyrdd (its fixture session) and a real Hub, behind
 * one same-origin proxy (hub/global-setup.ts). Needs Postgres and MinIO;
 * CI provides them (see the Studio end to end job).
 */
export default defineConfig({
  testDir: "./hub",
  testMatch: /.*\.spec\.ts/,
  globalSetup: "./hub/global-setup.ts",
  // Automerge wasm and the socket handshake can be slow on a CI runner.
  expect: { timeout: 20_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  outputDir: "../../target/e2e-results-hub",
  use: { trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
