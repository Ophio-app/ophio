// SPDX-License-Identifier: Apache-2.0
import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { ControlTarget } from "../../protocol/types";
import { LivePicture } from "./LivePicture";

afterEach(cleanup);
const target: ControlTarget = {
  target_id: "screen", kind: "output", label: "Display", native_ref: "screen",
  geometry: { width: 1920, height: 1080, scale: 1, rotation_degrees: 0, revision: 1 },
  capture: { state: "available" }, input: { state: "available" },
};

it("zooming back out to the whole picture does not leave it shifted off the surface", () => {
  const canvas = createRef<HTMLCanvasElement>();
  render(<LivePicture target={target} canvas={canvas} retry={() => undefined}
    picture={{ at: Date.now(), revision: 1, connected: true, error: null }} />);
  fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
  fireEvent.click(screen.getByRole("button", { name: "Pan left" }));
  fireEvent.click(screen.getByRole("button", { name: "Pan up" }));
  expect(canvas.current?.parentElement?.style.transform).toBe("translate(64px, 64px) scale(1.5)");
  fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
  // At 1× the pan controls are gone and dragging no longer pans, so a leftover
  // offset would hide the desktop's right and bottom edges with no way back
  // except Fit picture.
  expect(screen.queryByRole("button", { name: "Pan left" })).toBeNull();
  expect(canvas.current?.parentElement?.style.transform).toBe("translate(0px, 0px) scale(1)");
});

it("a late picture says input is paused on the picture, without pushing the controls down", () => {
  render(<LivePicture target={target} canvas={createRef()} retry={() => undefined}
    picture={{ at: Date.now() - 5000, revision: 1, connected: true, error: null }} />);
  const paused = screen.getByText("Agent paused");
  expect(paused.closest(".picture-surface")).not.toBeNull();
  expect(screen.getByText(/^Stale · \d+ s$/)).toBeTruthy();
});
