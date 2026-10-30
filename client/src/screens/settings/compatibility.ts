// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import type { ProtocolVersion } from "../../protocol/types";
export const versionText = (v: ProtocolVersion | null | undefined): string => v ? `${v.major}.${v.minor}` : "Not reported";
export function compatibility(app: ProtocolVersion | null, negotiated: ProtocolVersion | null, failed: boolean, detail?: string): string {
  if (failed) return `${detail || `This app and ${PRODUCT_NAME} on the computer can’t use this protocol together. Update the older installation from its original source.`} Drafts, Inbox and downloaded files still work on this device. Remote actions are unavailable.`;
  if (!negotiated) return "No protocol has been negotiated for this connection yet. Local drafts, Inbox and downloaded files remain available.";
  if (app && negotiated.major !== app.major) return `${negotiated.major > app.major ? "Update the app" : `Update ${PRODUCT_NAME} on the computer`} to connect. Local drafts, Inbox and downloaded files remain available.`;
  if (app && negotiated.minor < app.minor) return `Connected using an older protocol supported by both sides. Update ${PRODUCT_NAME} on the computer for newer features. Shared capabilities remain available.`;
  return `This app and ${PRODUCT_NAME} on the computer agreed on this protocol. Features are available only where the computer reports support.`;
}
/** Where the app came from; `updates` when the paired computer's update shows below. */
export function installerWords(installer: string | null | undefined, updates = true): string {
  if (installer === "com.android.vending") return "Installed from Google Play. Updates arrive through Google Play.";
  if (installer === "com.amazon.venezia") return "Installed from Amazon Appstore. Updates arrive through that store.";
  if (installer === "com.sec.android.app.samsungapps") return "Installed from Galaxy Store. Updates arrive through that store.";
  if (installer === "org.fdroid.fdroid") return "Installed from F-Droid. Updates arrive through F-Droid.";
  return `Installed outside an app store. Update it from the paired computer${updates ? ", as below," : ""} or install a new package from the same source.`;
}
