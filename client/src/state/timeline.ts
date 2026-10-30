// SPDX-License-Identifier: Apache-2.0
import type { DeliveryState, TimelineItem } from "../protocol/types";

type Latest = Map<number, { seq: number; state: DeliveryState; withdrawn: boolean; revoked: boolean }>;

function latestDeliveries(items: TimelineItem[]): Latest {
  const latest: Latest = new Map();
  for (const { seq, item } of items) {
    if (item.kind !== "delivery" || item.message_seq >= seq) continue;
    if ((latest.get(item.message_seq)?.seq ?? -1) < seq)
      latest.set(item.message_seq, { seq, state: item.state, withdrawn: item.withdrawn_by_stop === true, revoked: item.withdrawn_by_revocation === true });
  }
  return latest;
}

/** Keep delivery events for pagination/replay, while exposing the latest
 * recorded state on every loaded user message. Never mutate journal items. */
export function applyDeliveries(items: TimelineItem[]): TimelineItem[] {
  const latest = latestDeliveries(items);
  return items.map((it) => {
    const update = latest.get(it.seq);
    return it.item.kind === "user_message" && update && it.item.delivery !== update.state
      ? { ...it, item: { ...it.item, delivery: update.state } }
      : it;
  });
}

/** Messages Stop withdrew while they were still held, so none was written. */
export function withdrawnByStop(items: TimelineItem[]): Set<number> {
  return new Set([...latestDeliveries(items)].filter(([, d]) => d.state === "cancelled" && d.withdrawn).map(([seq]) => seq));
}

/** Messages withdrawn while held because the chat that sent them lost access. */
export function withdrawnByRevocation(items: TimelineItem[]): Set<number> {
  return new Set([...latestDeliveries(items)].filter(([, d]) => d.state === "cancelled" && d.revoked).map(([seq]) => seq));
}
