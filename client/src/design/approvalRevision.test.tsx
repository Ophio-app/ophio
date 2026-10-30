// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { approvals } from "../dev/sim/scenario";
import { CommandError } from "../host/session";
import { ApprovalCard } from "./ApprovalCard";
afterEach(cleanup);

it("offers the decision again when the request it went stale against comes back pending at a newer revision", async () => {
  const first = approvals(Date.now())[0]!;
  // Another device's decision was admitted first; its hand-off to the agent then
  // failed, so the computer re-opened the request at revision 2.
  const onDecide = vi.fn()
    .mockRejectedValueOnce(new CommandError({ code: "already_resolved", message: "Approval decision already admitted" }, null))
    .mockResolvedValue(undefined);
  const card = (approval = first) => <ApprovalCard approval={approval} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />;
  const { rerender } = render(card());
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Allow" })); });
  expect(screen.getByText("Request changed")).toBeTruthy();
  const reopened = { ...first, revision: 2, delivery: { decision: { decision: "allow" as const }, at: Date.now(), error: { code: "native_failure" as const, message: "The agent did not receive this. It may still be waiting." } } };
  rerender(card(reopened));
  expect(screen.queryByText("Request changed")).toBeNull();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Allow" })); });
  expect(onDecide).toHaveBeenCalledTimes(2);
});
