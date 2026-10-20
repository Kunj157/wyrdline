import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

import { serviceWorker } from "./service-worker";

export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    serviceWorker(fileURLToPath(new URL("./public", import.meta.url))),
  ],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  server: {
    port: 5717,
    proxy: { "/v1": "http://127.0.0.1:7717" },
  },
  build: { target: "es2023", sourcemap: true },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
});
