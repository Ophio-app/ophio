// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME, PROGRAM_NAME } from "../../brand";
import type { Platform } from "../../host/link";

function webUrl(value: unknown): string | null {
  if (typeof value !== "string" || !value.trim()) return null;
  try { const url = new URL(value.trim()); return ["https:", "http:"].includes(url.protocol) && !url.username && !url.password ? url.href : null; }
  catch { return null; }
}
export function companionDownload() {
  return { url: webUrl(import.meta.env.VITE_OPHIO_DOWNLOAD_URL), checksum: webUrl(import.meta.env.VITE_OPHIO_CHECKSUM_URL) };
}
export function companionSteps(config = companionDownload()): string {
  return [
    `Install ${PRODUCT_NAME} on your computer`,
    config.url ? `Download the Linux or Windows archive: ${config.url}` : `This build has no download link for ${PRODUCT_NAME}. Ask the person who supplied this app for the archive.`,
    ...(config.checksum ? [`Check the archive checksum: ${config.checksum}`] : []),
    "1. Extract the archive and follow INSTALL.md. Keep both executables together.",
    `2. Linux: the installer starts setup when it finishes; otherwise run "$HOME/.local/bin/${PROGRAM_NAME}" setup in your ordinary graphical login session. Windows: in PowerShell in the extracted folder, not as administrator, run .\\${PROGRAM_NAME}.exe setup. Setup starts ${PRODUCT_NAME} now and whenever you sign in.`,
    `3. Go on to setup's phone step, or run ${PROGRAM_NAME} pair (Windows: .\\${PROGRAM_NAME}.exe pair). Keep both devices on the same private network.`,
    "4. On your phone, choose Pair a computer and scan the code or paste the pairing link. The code expires after a few minutes.",
    "5. The computer asks whether to pair the phone. Confirm only if it shows the same code as the phone.",
  ].join("\n\n");
}
export async function shareCompanionSteps(platform: Pick<Platform, "shareText">): Promise<"shared" | "copied"> {
  const text = companionSteps();
  if (platform.shareText) return platform.shareText(text);
  if (navigator.share && (!navigator.canShare || navigator.canShare({ text }))) {
    await navigator.share({ text });
    return "shared";
  }
  await navigator.clipboard.writeText(text);
  return "copied";
}
