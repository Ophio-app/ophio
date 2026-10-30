// SPDX-License-Identifier: Apache-2.0
//
// A button that already says what to send, such as Start on work an agent
// found or Carry out this plan, sends it rather than leaving it in a draft.
// It goes through Review, which sends at once unless something there needs
// choosing, so every check Review makes still applies. Kept in memory only:
// after a restart nothing sends on its own.

import { draftKey } from "../../app/ui";

const asked = new Set<string>();

/** The next Review of this draft sends as soon as nothing stands in its way. */
export function sendOnOpen(projectId: string, taskId: string | null): void {
  asked.add(draftKey(projectId, taskId));
}

/** Whether a button asked this draft's Review to send it, without taking the ask. */
export const sendAsked = (projectId: string, taskId: string | null): boolean => asked.has(draftKey(projectId, taskId));

/** Takes the ask, so it sends at most once. */
export function takeSendAsk(projectId: string, taskId: string | null): void {
  asked.delete(draftKey(projectId, taskId));
}
