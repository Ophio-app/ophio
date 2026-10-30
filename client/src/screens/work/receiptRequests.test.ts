// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { TimelineItem } from "../../protocol/types";
import { receiptRequests } from "./receiptRequests";
it("associates only the exact queued message, across gaps and interleaved runs", () => {
  const message = (seq: number, run_id: string | null): TimelineItem => ({ seq, run_id, at: seq, item: { kind: "user_message", text: `${seq}`, bundle: null, delivery: "host_accepted" } });
  const items: TimelineItem[] = [message(1, null), message(2, null), message(3, "other"), message(4, "selected"),
    { seq: 50, at: 50, run_id: "selected", item: { kind: "delivery", message_seq: 2, state: "native_delivered" } }];
  expect(receiptRequests(items, "selected").map(i => i.seq)).toEqual([2, 4]);
  expect(receiptRequests(items, "selected")[0]?.item.delivery).toBe("native_delivered");
  expect(receiptRequests(items, "other").map(i => i.seq)).toEqual([3]);
});
