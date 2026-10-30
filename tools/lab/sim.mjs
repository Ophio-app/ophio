// SPDX-License-Identifier: Apache-2.0
// A symlinked node_modules resolves font assets outside the checkout; allow them.
import { createServer } from "../../client/node_modules/vite/dist/node/index.js";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../client/", import.meta.url));
const server = await createServer({
  root,
  mode: "sim",
  server: {
    host: "127.0.0.1",
    port: 5892,
    strictPort: true,
    fs: { allow: [realpathSync(root), realpathSync(root + "node_modules")] },
  },
});
await server.listen();
server.printUrls();
