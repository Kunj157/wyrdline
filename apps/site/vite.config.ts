import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Published at https://kunj157.github.io/wyrdline/; WYRD_SITE_BASE overrides
// it for local previews.
export default defineConfig({
  base: process.env.WYRD_SITE_BASE ?? "/wyrdline/",
  plugins: [react(), tailwindcss()],
  server: { port: 5718 },
  build: { target: "es2023", sourcemap: true },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
});
