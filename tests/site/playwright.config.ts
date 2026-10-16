import { defineConfig, devices } from "@playwright/test";

/**
 * The landing page as published: `scripts/build-site.sh` output (the site
 * with the real Studio and the recorded demo under studio/), served under
 * the GitHub Pages prefix /wyrdline/.
 */
export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  outputDir: "../../target/site-results",
  use: {
    baseURL: "http://127.0.0.1:4733",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "pnpm --filter @wyrdline/site exec vite preview --port 4733 --strictPort",
    url: "http://127.0.0.1:4733/wyrdline/",
    reuseExistingServer: false,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
