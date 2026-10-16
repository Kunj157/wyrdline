import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Published at https://kunj157.github.io/wyrdline/; WYRD_SITE_BASE overrides
// it for local previews.
export default defineConfig({
  base: process.env.WYRD_SITE_BASE ?? "/wyrdline/",
  plugins: [react(), tailwindcss()],
  server: { port: 5718 },
  // The lazy 3D chunk is ~140 kB gzip, inside the 250 kB budget (plan §16.1b).
  build: { target: "es2023", sourcemap: true, chunkSizeWarningLimit: 600 },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
});
