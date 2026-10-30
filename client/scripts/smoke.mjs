// SPDX-License-Identifier: Apache-2.0
//
// Browser smoke checks: the main journeys in the simulated client, each in
// portrait, landscape and at 200 % text both ways, run strictly by shots.mjs.
//
//   npm run smoke [-- <out-dir>]
//
// Starts its own simulator on a free loopback port and stops it afterwards.
// First it runs scripts/smoke/canary.json, whose shots each break one rule
// (a console error, a page error, overflow, two page titles, missing text, a
// stale click, a covered control, a control below the first screen, a
// toggle that is slow to show the tap, an unknown step); every one must
// fail, or the checks could not catch anything and the run fails. Then it runs
// scripts/smoke/plan.json. Screenshots and logs go to <out-dir>, by default a
// new folder under the temporary directory. The exit status is 0 only when
// the canaries all failed and every journey passed.
//
// plan.json: "variants" are the screen shapes every scenario runs in;
// "parts" are named step lists a scenario includes with ["part", name];
// "scenarios" are shots as shots.mjs takes them; one with "shapes" runs only
// in the variants it names.

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const client = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = process.argv[2] ?? mkdtempSync(join(tmpdir(), "ophio-smoke-"));
mkdirSync(out, { recursive: true });

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

// Runs a command, keeps its output and resolves with its exit status.
function run(args, label) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, args, { cwd: client, stdio: ["ignore", "pipe", "pipe"] });
    let log = "";
    child.stdout.on("data", (d) => (log += d));
    child.stderr.on("data", (d) => (log += d));
    child.on("error", (e) => resolve({ status: 1, log: `${label}: ${e}\n` }));
    child.on("close", (code, signal) => resolve({ status: code ?? (signal ? 1 : 0), log }));
  });
}

function expand(plan) {
  const steps = (list, seen = []) =>
    list.flatMap((step) => {
      if (step[0] !== "part") return [step];
      const name = step[1];
      if (!plan.parts?.[name]) throw new Error(`plan.json: no part named "${name}"`);
      if (seen.includes(name)) throw new Error(`plan.json: part "${name}" includes itself`);
      return steps(plan.parts[name], [...seen, name]);
    });
  return plan.variants.map((v) => ({
    suffix: v.suffix,
    shots: plan.scenarios
      .filter((s) => !s.shapes || s.shapes.includes(v.suffix))
      .map(({ shapes: _, ...s }) => {
        const { suffix: __, ...shape } = v;
        return { settle: 600, ...shape, ...s, name: `${s.name}-${v.suffix}`, steps: steps(s.steps) };
      }),
  }));
}

async function main() {
  const plan = expand(JSON.parse(readFileSync(join(client, "scripts/smoke/plan.json"), "utf8")));
  const port = await freePort();
  const vite = spawn(join(client, "node_modules/.bin/vite"), ["--mode", "sim", "--port", String(port), "--strictPort", "--host", "127.0.0.1"], {
    cwd: client,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let viteLog = "";
  vite.stdout.on("data", (d) => (viteLog += d));
  vite.stderr.on("data", (d) => (viteLog += d));
  const exited = new Promise((resolve) => vite.on("close", resolve));
  // The simulator needs a localhost address; 127.0.0.1 has no app bridge.
  const base = `http://localhost:${port}`;
  try {
    let up = false;
    for (let i = 0; i < 120 && !up && vite.exitCode === null; i++) {
      up = await fetch(base + "/").then((r) => r.ok, () => false);
      if (!up) await new Promise((r) => setTimeout(r, 250));
    }
    if (!up) {
      console.error(`The simulator did not start:\n${viteLog}`);
      return 1;
    }

    const canary = await run(["scripts/shots.mjs", "--strict", base, join(out, "canary"), "scripts/smoke/canary.json"], "canary");
    writeFileSync(join(out, "canary.log"), canary.log);
    const names = JSON.parse(readFileSync(join(client, "scripts/smoke/canary.json"), "utf8")).map((s) => s.name);
    const caught = names.filter((n) => new RegExp(`^FAIL ${n} `, "m").test(canary.log));
    const missed = names.filter((n) => !caught.includes(n));
    if (canary.status === 0 || missed.length) {
      console.error(`The checks can't catch failures: ${missed.length ? `these canaries passed: ${missed.join(", ")}` : "the canary run exited 0"}.\n${canary.log}`);
      return 1;
    }
    console.log(`canaries: all ${names.length} failed, as they should`);

    // The screen shapes run side by side; each has its own browser.
    const results = await Promise.all(
      plan.map(async ({ suffix, shots }) => {
        const file = join(out, `plan-${suffix}.json`);
        writeFileSync(file, JSON.stringify(shots, null, 1));
        const r = await run(["scripts/shots.mjs", "--strict", base, join(out, suffix), file], suffix);
        writeFileSync(join(out, `${suffix}.log`), r.log);
        return { suffix, ...r };
      }),
    );
    for (const r of results) process.stdout.write(r.log);
    const failed = results.filter((r) => r.status !== 0);
    const total = plan.reduce((n, v) => n + v.shots.length, 0);
    const fails = results.reduce((n, r) => n + (r.log.match(/^FAIL /gm)?.length ?? 0), 0);
    console.log(failed.length ? `smoke: ${fails} of ${total} failed (${failed.map((r) => r.suffix).join(", ")}); see ${out}` : `smoke: all ${total} passed; screenshots in ${out}`);
    return failed.length ? 1 : 0;
  } finally {
    vite.kill("SIGTERM");
    const timer = setTimeout(() => vite.kill("SIGKILL"), 5000);
    await exited;
    clearTimeout(timer);
  }
}

main().then(
  (status) => process.exit(status),
  (e) => {
    console.error(String(e?.stack ?? e));
    process.exit(1);
  },
);
