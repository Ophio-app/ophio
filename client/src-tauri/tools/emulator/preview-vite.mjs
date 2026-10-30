// SPDX-License-Identifier: Apache-2.0
// Simulator and Vite reload fixture; also serves fonts from a symlinked node_modules.
import { createServer } from "vite";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
const client = fileURLToPath(new URL("../../../", import.meta.url));
const server = await createServer({
  root: client,
  mode: "sim",
  server: {
    host: process.env.PREVIEW_TEST_HOST ?? "127.0.0.1",
    port: 5890,
    strictPort: true,
    fs: {
      allow: [
        client,
        realpathSync(new URL("../../../node_modules", import.meta.url)),
        fileURLToPath(
          new URL("../../../../target/preview-fixture", import.meta.url),
        ),
      ],
    },
  },
});
await server.listen();
server.printUrls();
for (const signal of ["SIGINT", "SIGTERM"])
  process.once(signal, async () => {
    await server.close();
    process.exit(0);
  });
