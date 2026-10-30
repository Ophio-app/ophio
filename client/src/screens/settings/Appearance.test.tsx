// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { prefs } from "../../app/prefs";
import { Appearance } from "./Appearance";

const initial = prefs.get();
afterEach(() => { cleanup(); prefs.set(initial); });

it("exposes a saved motion preference without changing the selected theme", () => {
  prefs.set({ ...initial, theme: "dark", reduceMotion: false });
  render(<Appearance />);
  const toggle = screen.getByRole("checkbox", { name: "Reduce motion" }) as HTMLInputElement;
  expect(toggle.checked).toBe(false);
  expect(document.getElementById(toggle.getAttribute("aria-describedby")!)?.textContent).toContain("system’s reduced-motion setting");
  fireEvent.click(toggle);
  expect(toggle.checked).toBe(true);
  expect(prefs.get().theme).toBe("dark");
  expect(document.documentElement.dataset.reduceMotion).toBe("true");
  expect(JSON.parse(localStorage.getItem("prefs")!).reduceMotion).toBe(true);
  fireEvent.click(toggle);
  expect(prefs.get().reduceMotion).toBe(false);
});

it("changing the theme preserves the saved motion preference", () => {
  prefs.set({ ...initial, theme: "dark", reduceMotion: true });
  render(<Appearance />);
  fireEvent.click(screen.getByRole("radio", { name: "Light" }));
  expect(prefs.get()).toMatchObject({ theme: "light", reduceMotion: true });
  expect(document.documentElement.dataset.theme).toBe("light");
  expect(document.documentElement.dataset.reduceMotion).toBe("true");
});
