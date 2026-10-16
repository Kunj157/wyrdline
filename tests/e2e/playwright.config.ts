import { defineConfig, devices } from "@playwright/test";

/**
 * Studio end to end against a real wyrdd serving the built Studio and a
 * fixture session (global-setup.ts). The webgpu project runs headed on a
 * virtual display (xvfb-run) with SwiftShader; it is enabled by
 * WYRD_E2E_WEBGPU=1.
 */
const webgpu = process.env.WYRD_E2E_WEBGPU === "1";

export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  globalSetup: "./global-setup.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  outputDir: "../../target/e2e-results",
  use: { trace: "retain-on-failure", screenshot: "only-on-failure" },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
      testIgnore: /perf-webgpu\.spec\.ts/,
    },
    ...(webgpu
      ? [
          {
            name: "webgpu",
            testMatch: /(river|perf-webgpu)\.spec\.ts/,
            use: {
              ...devices["Desktop Chrome"],
              headless: false,
              launchOptions: {
                args: [
                  "--enable-unsafe-webgpu",
                  "--enable-features=Vulkan",
                  "--use-vulkan=swiftshader",
                  "--use-webgpu-adapter=swiftshader",
                ],
              },
            },
          },
        ]
      : []),
  ],
});
