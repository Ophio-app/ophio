// SPDX-License-Identifier: Apache-2.0
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { once } from "node:events";
import { setTimeout } from "node:timers/promises";

const client = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(client, "..");
const data = await mkdtemp(join(tmpdir(), "ophio-intake-e2e-"));
let server;
const run = async (command, args, options = {}) => {
  const child = spawn(command, args, { cwd: repo, stdio: "inherit", ...options });
  const [code, signal] = await once(child, "exit");
  if (code !== 0) throw new Error(`${command} failed (${signal ?? code})`);
};
const freePort = async () => {
  const socket = createServer();
  socket.listen(0, "127.0.0.1");
  await once(socket, "listening");
  const port = socket.address().port;
  await new Promise((resolve, reject) => socket.close(error => error ? reject(error) : resolve()));
  return port;
};
const stop = () => { server?.kill("SIGTERM"); };
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
try {
  await run("cargo", ["build", "--offline", "--locked", "-p", "intake", "--bin", "ophio-intake"]);
  const metadata = spawn("cargo", ["metadata", "--offline", "--locked", "--no-deps", "--format-version", "1"], { cwd: repo, stdio: ["ignore", "pipe", "inherit"] });
  let output = "";
  metadata.stdout.setEncoding("utf8").on("data", chunk => { output += chunk; });
  const [code] = await once(metadata, "exit");
  if (code !== 0) throw new Error("Cargo metadata failed");
  const binary = join(JSON.parse(output).target_directory, "debug", "ophio-intake");
  await run(binary, ["init", "--data", data]);
  const token = (await readFile(join(data, "maintainer.token"), "utf8")).trim();
  const reporterPort = await freePort();
  let maintainerPort = await freePort();
  while (maintainerPort === reporterPort) maintainerPort = await freePort();
  const url = `http://127.0.0.1:${reporterPort}`;
  const privateUrl = `http://127.0.0.1:${maintainerPort}`;
  server = spawn(binary, ["serve", "--data", data, "--listen", `127.0.0.1:${reporterPort}`, "--maintainer-listen", `127.0.0.1:${maintainerPort}`], { cwd: repo, stdio: ["ignore", "ignore", "inherit"], env: { ...process.env, RUST_LOG: "error" } });
  await once(server, "spawn");
  const deadline = Date.now() + 10_000;
  for (;;) {
    if (server.exitCode !== null || server.signalCode !== null) throw new Error("Intake stopped before becoming ready");
    try {
      const response = await fetch(privateUrl + "/maintainer/v1/state", { headers: { authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(1000) });
      if (response.ok) break;
    } catch { /* Wait only for process startup, never for report polling. */ }
    if (Date.now() >= deadline) throw new Error("Intake did not become ready");
    await setTimeout(25);
  }
  await run("npm", ["test", "--", "src/diagnostics/intakeClient.e2e.test.ts"], { cwd: client, env: {
    ...process.env, OPHIO_INTAKE_E2E_URL: url, OPHIO_INTAKE_E2E_MAINTAINER_URL: privateUrl, OPHIO_INTAKE_E2E_TOKEN: token,
  } });
  console.log("Intake E2E: phone submission, status, question, withdrawal and CORS passed.");
} finally {
  if (server?.pid && server.exitCode === null && server.signalCode === null) {
    const exited = once(server, "exit");
    stop();
    await exited;
  }
  await rm(data, { recursive: true, force: true });
  process.removeListener("SIGINT", stop);
  process.removeListener("SIGTERM", stop);
}
