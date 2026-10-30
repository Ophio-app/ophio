import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

declare const process: { env: Record<string, string | undefined> };

// Tauri sets TAURI_DEV_HOST when a phone/emulator loads the dev server.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(({ mode }) => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5183 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2022",
    sourcemap: mode !== "production",
    // The simulated companion must never ship; see src/host/index.ts.
    rollupOptions: {},
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}", "src-tauri/tests/**/*.test.ts"],
  },
}));
