// SPDX-License-Identifier: Apache-2.0
import { ownership as describe, type OwnershipInfo } from "../../design/Ownership";
import type { TargetControlView } from "../../protocol/types";
import type { HostState } from "../../state/projection";

/** Cached ownership is only a claim while the connection is catching up. */
export function controlStateKnown(online: boolean, caughtUp: boolean): boolean {
  return online && caughtUp;
}

export function navigationInputState(state: HostState, online: boolean): "held" | "unknown" | "idle" {
  const views = Object.values(state.control);
  if (controlStateKnown(online, state.caughtUp) && views.some(v => ["human", "agent"].includes(v.input_state) && v.lease?.state === "active" && v.lease.boot_epoch === state.bootEpoch)) return "held";
  if (views.some(v => ["unknown", "revoking", "human", "agent"].includes(v.input_state))) return "unknown";
  return "idle";
}
export function ownership(view: TargetControlView | undefined, device: string | null, agent: (run: string) => string): OwnershipInfo {
  const info = describe(view, device, agent);
  return info.state === "paused" ? { ...info, detail: "Input stays off until you choose who controls it next." } : info;
}
