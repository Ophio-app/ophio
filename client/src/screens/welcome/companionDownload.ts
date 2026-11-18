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
/** The one-line installers in the project's own source, offered by builds that link to its official download page. */
export const ONE_LINE_INSTALL = {
  unix: "curl -fsSL https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.sh | sh",
  windows: "irm https://raw.githubusercontent.com/ophio-app/ophio/main/scripts/get-ophio.ps1 | iex",
};
export function oneLineInstall(config = companionDownload()) {
  return config.url && new URL(config.url).origin === "https://ophio.app" ? ONE_LINE_INSTALL : null;
}
export function companionSteps(config = companionDownload()): string {
  const quick = oneLineInstall(config);
  return [
    `Install ${PRODUCT_NAME} on your computer`,
    ...(quick ? [
      `Quickest: one command that downloads ${PRODUCT_NAME}, checks it and installs it. In a terminal on Linux or a Mac: ${quick.unix}`,
      `In PowerShell on Windows, not as administrator: ${quick.windows}`,
      "Or by hand:",
    ] : []),
    config.url ? `Download the archive for your computer: ${config.url}` : `This build has no download link for ${PRODUCT_NAME}. Ask the person who supplied this app for the archive.`,
    ...(config.checksum ? [`Check the archive checksum: ${config.checksum}`] : []),
    "1. Extract the archive and follow INSTALL.md. Keep both executables together.",
    `2. Linux and Mac: the installer starts setup when it finishes; otherwise run "$HOME/.local/bin/${PROGRAM_NAME}" setup as your ordinary user, signed in to the desktop. Windows: once INSTALL.md's install has finished, run & "$env:LOCALAPPDATA\\Programs\\Ophio\\${PROGRAM_NAME}.exe" setup in PowerShell, not as administrator. Setup starts ${PRODUCT_NAME} now and whenever you sign in.`,
    `3. Go on to setup's phone step, or run ${PROGRAM_NAME} pair. If the command isn't found, use the full path from step 2 with pair in place of setup. Keep both devices on the same private network.`,
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
