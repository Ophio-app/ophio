// SPDX-License-Identifier: Apache-2.0
import { defineConfig } from "vitest/config";
export default defineConfig({ test: { environment: "jsdom", include: ["src-tauri/tests/**/*.test.ts"] } });
