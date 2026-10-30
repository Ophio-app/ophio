// SPDX-License-Identifier: Apache-2.0
// Headless checks serve font assets through node_modules even when it is a symlink.
import { realpathSync } from "node:fs";
import { cwd } from "node:process";
import { defineConfig } from "vite";
import base from "../../../vite.config.ts";
export default defineConfig(async (env) => {
  const config = typeof base === "function" ? await base(env) : await base;
  return {
    ...config,
    server: {
      ...config.server,
      fs: { allow: [cwd(), realpathSync("node_modules")] },
    },
  };
});
