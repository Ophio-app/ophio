// SPDX-License-Identifier: Apache-2.0
import type { RunOptions } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import type { HostSession } from "../../host/session";
import type { Command, CommandReceipt, TargetControlView } from "../../protocol/types";
import { releaseHuman } from "../control/useControl";
import { handBackHistory } from "./handBackHistory";

/**
 * Give a waiting target's input back to the task's run, ending this device's
 * own control first. True once the computer confirmed it; false when the
 * request didn't leave, which `execute` has already said. Throws when the
 * computer refused it.
 */
export async function returnToAgent(
  session: HostSession,
  execute: (body: Command, opts: RunOptions) => Promise<CommandReceipt | null>,
  back: { taskId: string; runId: string; targetId: string; view: TargetControlView | undefined; mine: boolean },
): Promise<boolean> {
  if (back.mine && back.view?.lease) releaseHuman(session, back.view.lease);
  const receipt = await execute({ command: "return_to_agent", target_id: back.targetId, run_id: back.runId }, { label: "Resume agent requested" });
  if (!receipt) return false;
  const result = await commandResult(session, receipt);
  const command = session.commands.get()[receipt.command_id];
  const at = session.state.get().commandStages[receipt.command_id]?.at ?? Date.now();
  handBackHistory(session).set(records => [...records.filter(r => r.id !== receipt.command_id), {
    id: receipt.command_id, taskId: back.taskId, runId: back.runId, at,
    requestedAt: command?.issuedAt ?? at, history: session.commandHistory,
    summary: result?.kind === "intervention" ? result.receipt.summary : "Handed back",
  }].slice(-100));
  return true;
}
