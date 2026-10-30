// SPDX-License-Identifier: Apache-2.0
import { execFileSync } from "node:child_process";
// One emulator on the isolated build ADB server. Parallel runs choose theirs
// with ANDROID_SERIAL and OPHIO_PORT; never a physical device.
export const serial = process.env.ANDROID_SERIAL ?? "emulator-5562";
if (!/^emulator-\d+$/.test(serial))
  throw new Error("ANDROID_SERIAL must name an emulator.");
// The emulator reaches the computer's loopback at 10.0.2.2.
export const companionAddress = `10.0.2.2:${
  process.env.OPHIO_PORT ?? 47112
}`;
export function adb(...args) {
  if (process.env.ADB_SERVER_SOCKET !== "tcp:5038")
    throw new Error("Source the isolated build environment first.");
  return execFileSync("adb", ["-s", serial, ...args], {
    maxBuffer: 8 * 1024 * 1024,
  });
}
// A WebView's accessibility tree can lag its page by a moment, so a missing
// control is looked for again before giving up. A screen that never settles
// leaves no dump, rather than an earlier one saying where the control was.
export function tap(label) {
  let bounds;
  for (let attempt = 0; !bounds && attempt < 10; attempt++) {
    if (attempt) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 500);
    adb("shell", "rm", "-f", "/sdcard/shell-evidence.xml");
    // uiautomator is sometimes killed as it exits, even after dumping; a
    // failed dump is looked at again like an unsettled one.
    let dumped;
    try {
      dumped = adb("shell", "uiautomator", "dump", "/sdcard/shell-evidence.xml");
    } catch {
      continue;
    }
    if (!dumped.includes("dumped to")) continue;
    const xml = adb("exec-out", "cat", "/sdcard/shell-evidence.xml").toString();
    const node = [...xml.matchAll(/<node\s[^>]*>/g)]
      .map((m) => m[0])
      .find(
        (n) =>
          (n.includes(`text="${label}"`) ||
            n.includes(`resource-id="${label}"`) ||
            n.includes(`content-desc="${label}"`)) &&
          !n.includes('bounds="[0,0][0,0]"'),
      );
    bounds = node?.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  }
  if (!bounds) throw new Error(`Android control unavailable: ${label}`);
  adb(
    "shell",
    "input",
    "tap",
    String(Math.floor((Number(bounds[1]) + Number(bounds[3])) / 2)),
    String(Math.floor((Number(bounds[2]) + Number(bounds[4])) / 2)),
  );
}
