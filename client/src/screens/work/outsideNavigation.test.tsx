// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../../app/App";
import { useUi } from "../../app/context";
import { prefs } from "../../app/prefs";
import { platform } from "../../platform/lifecycle.fixtures";
import { objectLink } from "../../platform/links";
import { ids } from "../../dev/sim/scenario";
import { BottomNav } from "../../shell/BottomNav";
let fake: ReturnType<typeof platform>;
vi.mock("../../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../../shell/Shell", () => ({ Shell: function TestShell() {
  const route = useUi(s => s.stacks[s.tab].at(-1)?.screen);
  return <><p>Route {route}</p><BottomNav /></>;
} }));
beforeEach(() => {
  localStorage.clear(); prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  fake = platform();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);
it("opens an outside receipt on Tasks while Chat was current", async () => {
  render(<App />);
  await screen.findByText("Route talk");
  act(() => fake.lifecycle.controls.openLink(objectLink({ hostId: ids.host, kind: "task", id: ids.tHeaders })));
  fireEvent.click(await screen.findByRole("button", { name: "Open item" }));
  await screen.findByText("Route receipt");
  expect(screen.getByRole("button", { name: /^Tasks/ }).getAttribute("aria-current")).toBe("page");
  expect(screen.getByRole("button", { name: "Chat" }).getAttribute("aria-current")).toBeNull();
});
