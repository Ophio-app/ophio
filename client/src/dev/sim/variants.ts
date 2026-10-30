// SPDX-License-Identifier: Apache-2.0
// Connected desktop sessions with capture consent already granted. Protection
// failure mirrors services/runtime.rs; GNOME and KDE use the portal driver.
import type { HostCapabilities, HostInfo, ReadinessCheck } from "../../protocol/types";

const linuxStopReason = "Computer control needs the on-computer Stop. Run a desktop notification service that supports actions, such as mako or dunst.";
export const hostVariants = {
  windows: {
    os: "windows", backend: "windows", label: "Windows 11",
    stopReason: "Computer control needs the on-computer Stop, and the Windows notification area is not available. Sign in to the computer’s desktop.",
  },
  gnome: { os: "linux", backend: "portal", label: "Linux · GNOME", stopReason: linuxStopReason },
  kde: { os: "linux", backend: "portal", label: "Linux · KDE Plasma", stopReason: linuxStopReason },
  hyprland: { os: "linux", backend: "wlroots", label: "Arch Linux · Hyprland", stopReason: linuxStopReason },
} as const;
export type HostVariant = keyof typeof hostVariants;
export function parseHostVariant(value: string | null): HostVariant {
  return value && Object.hasOwn(hostVariants, value) ? value as HostVariant : "hyprland";
}
export function variantCapabilities(base: HostCapabilities, name: HostVariant, indicator = true): HostCapabilities {
  const variant = hostVariants[name];
  const protectedInput = indicator
    ? { state: "available" as const }
    : { state: "temporarily_unavailable" as const, reason: variant.stopReason };
  return {
    ...base,
    os: variant.os,
    desktop_backend: variant.backend,
    capture: { state: "available" },
    input: protectedInput,
    input_scope: "focused_target",
    computer_use: protectedInput,
    lock_session: { state: "available" },
    // As on the computer: only Linux checks a password to unlock.
    unlock_session: variant.os === "windows"
      ? { state: "unsupported", reason: "Windows can’t be unlocked from Ophio" }
      : { state: "available" },
    // Only Linux holds a conversation's agent to what it may reach, so far.
    conversation_access: variant.os === "windows"
      ? { state: "unsupported", reason: "Windows can’t keep a conversation to what it may reach yet" }
      : base.conversation_access,
    features: [...new Set([...base.features, "durable_admission", "journal_resume", "channel_link_review", "task_history"])],
  };
}
export function variantHost(base: HostInfo, name: HostVariant, indicator = true): HostInfo {
  return { ...base, os_label: hostVariants[name].label, capabilities: variantCapabilities(base.capabilities, name, indicator) };
}
export function variantChecks(name: HostVariant, indicator = true): ReadinessCheck[] {
  const variant = hostVariants[name];
  return [
    {
      area: "graphics", label: `Screen capture (${variant.backend})`, status: "passed",
      detail: variant.backend === "portal" ? "Desktop sharing permission granted" : "Desktop session connected", remedy: null,
    },
    {
      area: "graphics", label: "On-computer Stop", status: indicator ? "passed" : "failed",
      detail: indicator ? (variant.os === "windows" ? "Windows notification area" : "Desktop notification actions") : variant.stopReason, remedy: null,
    },
    {
      area: "graphics", label: "Input injection", status: indicator ? "passed" : "failed",
      detail: indicator ? "Input goes to the focused window" : variant.stopReason, remedy: null,
    },
  ];
}
