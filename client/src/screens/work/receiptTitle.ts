// SPDX-License-Identifier: Apache-2.0
import type { TimelineItem } from "../../protocol/types";

/** The computer titles an untitled task with its request's first characters. */
const TITLE_CUT = 80;
const MAX = 120;

/**
 * A receipt's heading has room for more than a task row. When the title was
 * cut from the request, the heading finishes the sentence it cut, up to MAX
 * characters. A title the person wrote is shown as written.
 */
export function receiptTitle(title: string, items: TimelineItem[]): string {
  const cut = title.endsWith("…") ? title.slice(0, -1).trimEnd() : [...title].length === TITLE_CUT ? title : null;
  const first = items.find(i => i.item.kind === "user_message")?.item;
  const request = first?.kind === "user_message" ? first.text?.trim() : undefined;
  if (!cut || !request?.startsWith(cut) || request.length === cut.length) return title;
  const end = request.slice(cut.length).search(/\n|[.!?](?=\s|$)/);
  const sentence = end < 0 ? request : request.slice(0, cut.length + end + (request[cut.length + end] === "\n" ? 0 : 1));
  if (sentence.length <= MAX) return sentence;
  const shown = sentence.slice(0, MAX - 1).replace(/\s+\S*$/, "");
  return shown.length > cut.length ? `${shown}…` : title;
}
