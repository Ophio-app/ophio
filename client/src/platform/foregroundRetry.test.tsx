// SPDX-License-Identifier: Apache-2.0
//
// While a computer is offline its link waits between attempts, up to 30 s.
// Coming back to the app is the moment a person looks, so it tries at once.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp } from "../app/context";
import { prefs } from "../app/prefs";
import { FakeLink, platform } from "./lifecycle.fixtures";

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  return <p>Computer {useApp().profile.name}</p>;
} }));
beforeEach(() => {
  localStorage.clear(); prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

it("tries again at once when the app returns while the computer is offline, and only then", async () => {
  const links: FakeLink[] = [];
  fake = platform(links);
  const foreground = new Set<() => void>();
  fake.value.onForeground = (listener) => { foreground.add(listener); return () => { foreground.delete(listener); }; };
  fake.value.control = () => { const l = new FakeLink(); l.offline = true; links.push(l); return l; };
  render(<App />);
  await screen.findByText("Computer studio");
  const link = links[0]!;
  const connect = vi.spyOn(link, "connect");
  expect(link.sent).toEqual([]);

  // The network is back; the app comes to the front before the next attempt.
  link.offline = false;
  act(() => { for (const listener of foreground) listener(); });
  expect(connect).toHaveBeenCalledTimes(1);

  // Online, returning to the app leaves the connection alone.
  act(() => { for (const listener of foreground) listener(); });
  expect(connect).toHaveBeenCalledTimes(1);
});
