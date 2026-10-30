// SPDX-License-Identifier: Apache-2.0
// The same timed normal-input probe, limited to the disposable guest and phone.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { connect, pause } from "../../client/src-tauri/tools/emulator/cdp.mjs";
import { lockPassed } from "./lock_result.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const [target] = process.argv.slice(2);
if (!["hyprland", "gnome", "plasma"].includes(target))
  throw Error("Choose a lab guest");
const label = process.env.LAB_EVIDENCE_RUN;
if (label && !/^[a-zA-Z0-9_-]{1,40}$/.test(label))
  throw Error("Invalid evidence run label");
const folder = path.join(root, "target/lab-evidence", label ?? "", target);
fs.mkdirSync(folder, { recursive: true, mode: 0o700 });
const guest = (args) =>
  execFileSync(
    "python3",
    [
      path.join(root, "tools/lab/lab.py"),
      "ssh",
      target,
      "bash",
      "lab/in-session.sh",
      ...args,
    ],
    { encoding: "utf8", timeout: 10000 },
  );
const c = await connect(Number(process.env.LAB_CDP_PORT ?? "9232"));
const result = { target, samples: [], lockConfirmed: false };
try {
  const armed = await c.evaluate(
    "typeof window.labSend === 'function' && document.body.innerText.includes('\\nLive\\n') && Object.values(labApp.session.state.get().control).some(v => v.input_state === 'human' && v.lease?.state === 'active')",
  );
  if (!armed)
    throw Error("Arm live phone control before requesting the guest lock");
  if (!await c.evaluate("Array.isArray(window.labAcks) && labAcks.length === 0"))
    throw Error("Arm fresh control without earlier acknowledgements before the lock probe");
  const start = performance.now();
  if (target === "hyprland") {
    guest([
      "sh",
      "-c",
      "nohup hyprlock --immediate-render --no-fade-in --config lab/hyprlock.conf > evidence/hyprlock.log 2>&1 < /dev/null &",
    ]);
    for (let attempt = 0; attempt < 20; attempt++) {
      await pause(50);
      result.lockConfirmed =
        JSON.parse(guest(["hyprctl", "-j", "locked"])).locked === true;
      if (result.lockConfirmed) break;
    }
  } else {
    const name =
      target === "gnome"
        ? "org.gnome.ScreenSaver"
        : "org.freedesktop.ScreenSaver";
    const object =
      target === "gnome" ? "/org/gnome/ScreenSaver" : "/ScreenSaver";
    guest([
      "gdbus",
      "call",
      "--session",
      "--dest",
      name,
      "--object-path",
      object,
      "--method",
      `${name}.Lock`,
    ]);
    for (let attempt = 0; attempt < 20; attempt++) {
      result.lockConfirmed = guest([
        "gdbus",
        "call",
        "--session",
        "--dest",
        name,
        "--object-path",
        object,
        "--method",
        `${name}.GetActive`,
      ]).includes("true");
      if (result.lockConfirmed) break;
      await pause(50);
    }
  }
  if (!result.lockConfirmed)
    throw Error("The desktop did not confirm its lock");
  result.lockConfirmedAtMs = performance.now() - start;
  try {
    result.sent = await c.evaluate("labSend({action:'text',text:'LOCK_TEST'})");
  } catch (error) {
    // A locked UI removes the picture and its normal input callback. Preserve
    // this refusal and continue the original timed observations.
    result.sent = false;
    result.inputError =
      error instanceof Error ? error.message : "Input callback unavailable";
  }
  for (const at of [500, 1500, 4000, 8000]) {
    await pause(Math.max(0, at - (performance.now() - start)));
    const sample = await c.evaluate(
      `({sent:null,acks:labAcks,control:labApp.session.state.get().control,text:document.body.innerText})`,
    );
    result.samples.push({ elapsedMs: performance.now() - start, ...sample });
  }
  if (!lockPassed(result))
    throw Error("Lock observations were not consistently paused, locked and input-free");
  result.status = "pass";
} catch (error) {
  result.status = "fail";
  result.error = error instanceof Error ? error.message : "Lock check failed";
  process.exitCode = 1;
} finally {
  fs.writeFileSync(
    path.join(folder, "lock-timed.json"),
    JSON.stringify(result, null, 2) + "\n",
    { mode: 0o600 },
  );
  c.close();
}
