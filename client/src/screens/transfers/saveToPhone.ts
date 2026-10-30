// SPDX-License-Identifier: Apache-2.0
import type { Platform } from "../../host/link";

/** Puts a verified download in the phone's storage and says where it went. */
export async function saveToPhone(
  blob: Blob,
  name: string,
  platform: Pick<Platform, "saveToPhone">,
): Promise<string> {
  if (!platform.saveToPhone)
    throw new Error(
      "Saving to device storage isn’t available in this build. The verified file is kept in the transfer tray.",
    );
  return (await platform.saveToPhone(blob, name)).savedAs;
}
