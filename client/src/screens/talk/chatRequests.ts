// SPDX-License-Identifier: Apache-2.0
//
// What the chat's heading and menu ask of its message box, which owns the
// sheets: the picker for the next message's agent, model and effort, and
// Turn into a task. One request at a time, kept until the open chat's message
// box takes it, and dropped if it isn't taken soon, so a chat opened later
// doesn't jump into a sheet.

import { useEffect } from "react";
import { useApp } from "../../app/context";
import type { UiStore } from "../../app/ui";
import { Store, useStore } from "../../state/store";

export type ChatAsk = "model" | "into_task";
type Request = { taskId: string; ask: ChatAsk; at: number };

const STALE_MS = 10_000;
const stores = new WeakMap<UiStore, Store<Request | null>>();

function store(ui: UiStore): Store<Request | null> {
  let s = stores.get(ui);
  if (!s) { s = new Store<Request | null>(null); stores.set(ui, s); }
  return s;
}

/** The key a new task or chat in `projectId` is asked under, before it has an id. */
export function newAsk(projectId: string): string {
  return `new:${projectId}`;
}

/** Asks the open chat's message box for one of its sheets. */
export function askChat(ui: UiStore, taskId: string, ask: ChatAsk): void {
  store(ui).set({ taskId, ask, at: Date.now() });
}

/** Runs `take` when the chat `taskId` is asked for a sheet. */
export function useChatAsk(taskId: string | undefined, take: (ask: ChatAsk) => void): void {
  const { ui } = useApp();
  const s = store(ui);
  const request = useStore(s, (r) => r);
  useEffect(() => {
    if (!taskId || request?.taskId !== taskId) return;
    s.set(null);
    if (Date.now() - request.at <= STALE_MS) take(request.ask);
  }, [request, taskId]);
}
