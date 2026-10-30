// SPDX-License-Identifier: Apache-2.0
import type { NativeReportScreenshot } from "../../platform/reportNative";
import { openReport } from "../../reports/request";
export const reportPhone = {
  capture: false,
  detection: false,
  listeners: new Set<() => void>(),
  screenshot() { for (const cb of this.listeners) cb(); },
  open() { openReport(); },
};
export function captureReportView(): NativeReportScreenshot | null {
  if (!reportPhone.capture) return null;
  const canvas = document.createElement("canvas"); canvas.width = 390; canvas.height = 700;
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = "#faf7f0"; ctx.fillRect(0, 0, 390, 700); ctx.fillStyle = "#211526"; ctx.font = "24px sans-serif"; ctx.fillText("Øphio", 24, 50); ctx.font = "18px sans-serif"; ctx.fillText("Tasks", 24, 110); ctx.fillText("Waiting for the computer", 24, 175); ctx.fillStyle = "#dcd1df"; ctx.fillRect(24, 205, 342, 1);
  return { media_type: "image/png", data: canvas.toDataURL("image/png").split(",")[1]!, width: 390, height: 700 };
}
