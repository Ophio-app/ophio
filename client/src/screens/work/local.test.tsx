// SPDX-License-Identifier: Apache-2.0
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useLocalText, useVisitCutoff } from "./local";
import { ApprovalCard } from "../../design/ApprovalCard";
import { approvals } from "../../dev/sim/scenario";

beforeEach(() => localStorage.clear());
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("shares edits, clears and save failures only with the same draft", () => {
  const first = renderHook(() => useLocalText("project-path"), { wrapper: StrictMode });
  const second = renderHook(() => useLocalText("project-path"), { wrapper: StrictMode });
  const other = renderHook(() => useLocalText("other-project-path"));
  act(() => first.result.current[1]("/home/you/garden"));
  expect(second.result.current[0]).toBe("/home/you/garden");
  const storage = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Storage is full"); });
  act(() => second.result.current[1]("/home/you/orchard"));
  expect(first.result.current[0]).toBe("/home/you/orchard");
  expect(first.result.current[2]).toBe(false);
  expect(second.result.current[2]).toBe(false);
  expect(other.result.current[0]).toBe("");
  expect(other.result.current[2]).toBe(true);
  storage.mockRestore();
  act(() => first.result.current[1](""));
  expect(second.result.current[0]).toBe("");
  expect(second.result.current[2]).toBe(true);
  expect(localStorage.getItem("work.project-path")).toBe('""');
});
describe("return visits", () => {
  it("does not consume the first brief during StrictMode effect replay", () => {
    const hook = renderHook(
      ({ active, at }) => useVisitCutoff("visit", active, () => at),
      {
        initialProps: { active: true, at: 10 },
        wrapper: StrictMode,
      },
    );
    expect(hook.result.current).toBe(0);
    hook.rerender({ active: true, at: 20 });
    expect(hook.result.current).toBe(0);
    hook.rerender({ active: false, at: 20 });
    expect(localStorage.getItem("work.visit")).toBe("20");
    hook.rerender({ active: true, at: 30 });
    expect(hook.result.current).toBe(20);
  });
  it("does not consume a later unseen event when the page closes", () => {
    renderHook(() => useVisitCutoff("visit", true, () => 50));
    act(() => window.dispatchEvent(new Event("pagehide")));
    expect(localStorage.getItem("work.visit")).toBe("50");
  });
});
describe("approval draft contract", () => {
  it("restores an unsent answer and disables decisions while offline", () => {
    const decision = vi.fn();
    function Answer({ offline = false }: { offline?: boolean }) {
      const [text, onChange] = useLocalText("approval");
      const approval = approvals(Date.now())[0];
      if (!approval) throw new Error("Missing approval fixture");
      return (
        <ApprovalCard
          approval={{ ...approval, kind: "question" }}
          agent="Agent"
          myDeviceId={null}
          canDecide
          draft={{ text, onChange }}
          disabled={offline}
          disabledReason="Connect to answer"
          onDecide={decision}
        />
      );
    }
    const first = render(<Answer />);
    fireEvent.change(first.getByLabelText("Your answer"), {
      target: { value: "Keep the existing behavior" },
    });
    first.unmount();
    const next = render(<Answer offline />);
    expect(
      (next.getByLabelText("Your answer") as HTMLTextAreaElement).value,
    ).toBe("Keep the existing behavior");
    expect(
      (next.getByRole("button", { name: "Send answer" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(next.queryByText(/not granted approvals/)).toBeNull();
    expect(decision).not.toHaveBeenCalled();
  });
});

it("keeps a failed approval actionable and explains delivery after reopening", async () => {
  const approval = approvals(Date.now())[0]!;
  const error = { code: "native_failure" as const, message: "The agent did not receive this. It may still be waiting." };
  const onDecide = vi.fn().mockRejectedValue({ error });
  const props = { agent: "Agent", myDeviceId: null, canDecide: true, onDecide };
  const first = render(<ApprovalCard {...props} approval={approval} />);
  await act(async () => fireEvent.click(first.getByRole("button", { name: "Allow" })));
  expect(first.getByRole("alert").textContent).toBe(error.message);
  expect(first.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", false);
  first.unmount();
  const next = render(<ApprovalCard {...props} approval={{ ...approval, delivery: { decision: { decision: "allow" }, at: Date.now(), error } }} />);
  expect(next.getByRole("alert").textContent).toBe(error.message);
  expect(next.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", false);
  expect(next.queryByText(/agent received/)).toBeNull();
});
