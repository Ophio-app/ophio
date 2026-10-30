// SPDX-License-Identifier: Apache-2.0
//
// Chooses the platform. The simulated computer exists only in `--mode sim`
// builds; the condition is static so release builds never include it.

import type { Platform } from "./link";

export async function loadPlatform(): Promise<Platform> {
  if (import.meta.env.MODE === "sim") {
    const { createSimPlatform } = await import("../dev/sim/platform");
    return createSimPlatform();
  }
  const { createNativePlatform } = await import("./native");
  return createNativePlatform();
}
