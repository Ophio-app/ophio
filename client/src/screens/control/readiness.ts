// SPDX-License-Identifier: Apache-2.0
import type { Availability, ControlTarget, Grants, HostCapabilities, TargetControlView, Task } from "../../protocol/types";

/** This device can unlock the computer with its password: the computer can
 * check one, and the device has Control and Manage, which unlocking takes. */
export function canUnlockHere(caps: HostCapabilities | null, grants: Grants | null): boolean {
  return caps?.unlock_session?.state === "available" &&
    grants?.control_input !== false && grants?.admin !== false;
}

/**
 * The same native lock observation fences pictures and every input entry. An
 * unknown lock state fences only where the computer fences on it, which it
 * reports as input not being available; a computer without lock observation
 * always reports unknown and keeps its ordinary input policy. Every place
 * that says the computer is locked offers its unlock right there, so "here"
 * is wherever the person reads it.
 */
export function desktopBlocker(view: TargetControlView | undefined, caps: HostCapabilities | null,
  grants: Grants | null): string | null {
  if (view?.desktop_lock === "locked")
    return canUnlockHere(caps, grants)
      ? "Unlock it here with its password, or at the computer, to watch and control it again."
      : "Unlock on the computer to watch and control again.";
  if (view?.desktop_lock === "unknown" && caps?.input.state !== "available")
    return "The computer hasn’t confirmed its lock state. Check the desktop before continuing.";
  return null;
}

function unavailable(a: Availability | undefined, label: string): string | null {
  if (!a) return `The computer hasn’t reported ${label} readiness.`;
  if (a.state === "available") return null;
  if ("remedy" in a) return a.remedy;
  if ("reason" in a) return a.reason;
  return `${label} permission was removed on the computer.`;
}

export function inputBlocker(caps: HostCapabilities | null, target: ControlTarget | undefined,
  view: TargetControlView | undefined, grants: Grants | null): string | null {
  return desktopBlocker(view, caps, grants) ?? unavailable(caps?.input, "Input")
    ?? unavailable(caps?.capture, "Picture") ?? unavailable(target?.capture, "Display picture")
    ?? unavailable(target?.input, "Display input");
}

/** Talk and Review apply the same input fence when continuing computer use. */
export function continuationBlocker(task: Task | null | undefined, caps: HostCapabilities | null,
  targets: ControlTarget[], control: Record<string, TargetControlView>, grants: Grants | null): string | null {
  if (task?.kind !== "computer_use") return null;
  const target = targets.find((t) => t.target_id === task.target_id);
  return inputBlocker(caps, target, task.target_id ? control[task.target_id] : undefined, grants);
}
