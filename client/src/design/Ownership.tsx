// SPDX-License-Identifier: Apache-2.0
//
// Input ownership strip: who can move the pointer and type on this target,
// independent of whether a task is running or finished.

import { Bot, CircleHelp, Eye, Hand, Hourglass, Pause, Ban, type LucideIcon } from "lucide-react";
import type { InputState, TargetControlView } from "../protocol/types";
import { useLink } from "../app/context";
import { Age } from "./Age";

export interface OwnershipInfo {
  state: InputState;
  icon: LucideIcon;
  tone: "neutral" | "accent" | "agent" | "warn";
  title: string;
  detail: string;
}

export function ownership(view: TargetControlView | undefined, myDeviceId: string | null, agentName: (runId: string) => string): OwnershipInfo {
  const state: InputState = view?.input_state ?? "unknown";
  const lease = view?.lease ?? null;
  switch (state) {
    case "ready":
      return { state, icon: Eye, tone: "neutral", title: "Watching", detail: "No one is controlling input" };
    case "human": {
      const mine = lease?.owner.kind === "human" && lease.owner.device_id === myDeviceId;
      return mine
        ? { state, icon: Hand, tone: "accent", title: "You have control", detail: "Your taps and keys go to this screen" }
        : { state, icon: Hand, tone: "warn", title: "Another device has control", detail: "Input from here is off" };
    }
    case "agent": {
      const who = lease?.owner.kind === "agent" ? agentName(lease.owner.run_id) : "An agent";
      return { state, icon: Bot, tone: "agent", title: `${who} has control`, detail: "Pause the agent or take over at any time" };
    }
    case "revoking":
      return { state, icon: Hourglass, tone: "warn", title: "Stopping input", detail: "Requested; the computer has not confirmed yet" };
    case "paused":
      return { state, icon: Pause, tone: "warn", title: "Agent paused", detail: "It can’t use the screen until you resume it." };
    case "unavailable":
      return { state, icon: Ban, tone: "neutral", title: "Input unavailable", detail: "This screen can be watched but not controlled" };
    case "unknown":
      return { state, icon: CircleHelp, tone: "warn", title: "Input owner unknown", detail: view ? "The computer hasn’t confirmed who has control. Pause the agent before continuing." : "The computer hasn’t reported who has control yet" };
  }
}

export function OwnershipStrip({ info, since, className }: { info: OwnershipInfo; since?: number | null; className?: string }) {
  const link = useLink();
  if (link.state === "failed") info = {
    state: "unknown", icon: CircleHelp, tone: "warn",
    title: link.reason === "revoked" ? "Access removed" : "Access unavailable",
    detail: "Control state unknown on this device",
  };
  else if (link.state !== "online") info = {
    state: "unknown", icon: CircleHelp, tone: "warn", title: "Control state unknown on this device",
    detail: "Reconnect to check who has input. Input from this device is off.",
  };
  const Icon = info.icon;
  return (
    <div className={["ownership", `ownership--${info.tone}`, className].filter(Boolean).join(" ")} role="status" aria-live="polite">
      <Icon aria-hidden strokeWidth={2.1} className="ownership__icon" />
      <span className="ownership__text">
        <span className="ownership__title">{info.title}</span>
        <span className="ownership__detail">
          {info.detail}
          {since != null && (
            <>
              {" · "}
              <Age at={since} suffix=" ago" />
            </>
          )}
        </span>
      </span>
    </div>
  );
}
