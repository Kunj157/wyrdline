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
    // Bind IPv4 explicitly: on the runners "localhost" resolves to ::1.
    command:
      "pnpm --filter @wyrdline/site exec vite preview --host 127.0.0.1 --port 4733 --strictPort",
    stdout: "pipe",
    url: "http://127.0.0.1:4733/wyrdline/",
    reuseExistingServer: false,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
