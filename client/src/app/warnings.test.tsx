// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { prefs } from "./prefs";
import { hideWarning, showWarning, useWarning, warns, type WarningId } from "./warnings";

const initial = prefs.get();
beforeEach(() => prefs.set({ ...initial, warningsOff: false, hiddenWarnings: [] }));
afterEach(() => { cleanup(); prefs.set(initial); });

/** A button behind a warning, the way the app uses one. */
function Gated({ id, onGo }: { id: WarningId; onGo: () => void }) {
  const warn = useWarning(id);
  const [open, setOpen] = useState(false);
  return <>
    <button onClick={() => warn.ask(() => setOpen(true), onGo)}>Delete{warn.more}</button>
    {open && <div role="dialog" aria-label="Delete it?">
      {warn.check}
      <button onClick={() => setOpen(false)}>Cancel</button>
      <button onClick={() => { warn.agreed(); setOpen(false); onGo(); }}>Delete it</button>
    </div>}
  </>;
}

it("hides and shows one warning at a time, and turns every one off at once", () => {
  expect(warns("file.delete")).toBe(true);
  hideWarning("file.delete");
  hideWarning("file.delete");
  expect(prefs.get().hiddenWarnings).toEqual(["file.delete"]);
  expect(warns("file.delete")).toBe(false);
  expect(warns("terminal.close")).toBe(true);
  showWarning("file.delete");
  expect(warns("file.delete")).toBe(true);
  prefs.set((p) => ({ ...p, warningsOff: true }));
  expect(warns("file.delete")).toBe(false);
  expect(warns("terminal.close")).toBe(false);
});

it("asks first, and keeps “Don’t show me again” only when the person goes ahead", () => {
  let went = 0;
  render(<Gated id="file.delete" onGo={() => went++} />);
  fireEvent.click(screen.getByRole("button", { name: "Delete…" }));
  expect(went).toBe(0);
  fireEvent.click(screen.getByRole("checkbox", { name: "Don’t show me again" }));
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(prefs.get().hiddenWarnings).toEqual([]);

  // Opened again, the box starts clear.
  fireEvent.click(screen.getByRole("button", { name: "Delete…" }));
  const box = screen.getByRole("checkbox", { name: "Don’t show me again" }) as HTMLInputElement;
  expect(box.checked).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Delete it" }));
  expect(went).toBe(1);
  expect(prefs.get().hiddenWarnings).toEqual([]);

  fireEvent.click(screen.getByRole("button", { name: "Delete…" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Don’t show me again" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete it" }));
  expect(went).toBe(2);
  expect(prefs.get().hiddenWarnings).toEqual(["file.delete"]);

  // From then on it goes ahead at once, and the button loses its “…”.
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(went).toBe(3);
});

it("goes ahead at once while every warning is off", () => {
  let went = 0;
  render(<Gated id="window.close" onGo={() => went++} />);
  act(() => prefs.set((p) => ({ ...p, warningsOff: true })));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(went).toBe(1);
  expect(prefs.get().hiddenWarnings).toEqual([]);
});
