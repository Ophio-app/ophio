// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ReportService } from "../diagnostics/reports";
import { CrashRegistry } from "../diagnostics/crashes";
import { PhoneTrail } from "../diagnostics/trail";
import { describeBundle } from "../diagnostics/bundle";
import type { ReportScreenshot } from "../protocol/types";
import { ReportSheet } from "./ReportSheet";

const picture: ReportScreenshot = { data: "AA==", media_type: "image/png", width: 1, height: 1, marked: false };
vi.mock("./Markup", () => ({ pictureUrl: () => "data:image/png;base64,AA==", Markup: ({ onSave }: { onSave(p: ReportScreenshot): void }) => <button onClick={() => onSave({ ...picture, data: "AQ==", marked: true })}>Keep marks</button> }));
beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
async function draft() {
  const service = new ReportService({ crashes: new CrashRegistry(), trail: new PhoneTrail(), intake: { available: async () => false } as never });
  return { service, draft: await service.startReport({ type: "settings" }, { screen: "/settings" }) };
}
it("has no empty picture placeholder where capture is unavailable, and saves words before acknowledging", async () => {
  const d = await draft();
  render(<ReportSheet {...d} onClose={vi.fn()} />);
  expect(screen.queryByRole("img")).toBeNull();
  fireEvent.change(screen.getByLabelText("What happened?"), { target: { value: "The button did not respond." } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect(screen.getByText(/Got it, report/).textContent).toContain(d.draft.reference);
  expect(screen.getByText("Saved here. It will go when it can.")).toBeTruthy();
  expect(d.service.get().queued[0]?.bundle.words).toBe("The button did not respond.");
});
it("replaces the attachment with flattened markup and removes it on request", async () => {
  const d = await draft(); d.service.setScreenshot(d.draft, picture);
  render(<ReportSheet {...d} onClose={vi.fn()} />);
  expect(screen.getByAltText("Ophio’s screen when this report started")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Mark up this screen" }));
  fireEvent.click(screen.getByRole("button", { name: "Keep marks" }));
  expect(d.draft.bundle.screenshot).toEqual({ ...picture, marked: true, data: "AQ==" });
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  expect(d.draft.bundle.screenshot).toBeUndefined(); expect(screen.queryByRole("img")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Add the picture back" }));
  expect(d.draft.bundle.screenshot).toEqual({ ...picture, marked: true, data: "AQ==" });
  expect(screen.getByAltText("Ophio’s screen when this report started")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Add the picture back" })).toBeNull();
});
it("See all mirrors every reviewed leaf, and extras refresh that exact description", async () => {
  const d = await draft(); d.draft.phoneOnly = false;
  const extra = vi.spyOn(d.service, "toggleExtra").mockImplementation(async (draft, part, enabled) => {
    draft.extras = enabled ? [part] : [];
    draft.bundle.extras = enabled ? [{ type: "names", names: [{ pseudonym: "p:sample", name: "Sample" }] }] : [];
    draft.description = describeBundle(draft.bundle);
  });
  render(<ReportSheet {...d} taskId="task" onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "See all ›" }));
  for (const section of describeBundle(d.draft.bundle)) expect(screen.getByText(section.label, { selector: "summary" })).toBeTruthy();
  const toggle = screen.getByLabelText("Real names for the pseudonyms") as HTMLInputElement;
  expect(toggle.checked).toBe(false);
  fireEvent.click(toggle);
  expect(toggle.checked).toBe(true);
  expect(extra).toHaveBeenCalledWith(d.draft, { type: "names" }, true);
  expect(await screen.findByText("Sample")).toBeTruthy();
  expect(screen.getByLabelText("This task’s conversation")).toBeTruthy();
});
it("keeps Allow unanswered if the requested extra was switched off before sending", async () => {
  const d = await draft(), onSent = vi.fn();
  render(<ReportSheet {...d} requiredExtra={{ type: "names" }} onClose={vi.fn()} onSent={onSent} />);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect(screen.getByText(/Include the requested extra/)).toBeTruthy();
  expect(d.service.get().queued).toHaveLength(0); expect(onSent).not.toHaveBeenCalled();
});
