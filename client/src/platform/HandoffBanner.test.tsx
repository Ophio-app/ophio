// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp, useUi } from "../app/context";
import { prefs } from "../app/prefs";
import { UiStore } from "../app/ui";
import { FakeLink, platform, profile } from "./lifecycle.fixtures";
import { secondProfile } from "../dev/sim/lifecycle";
import * as fx from "../dev/sim/scenario";

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const route = useUi(s => s.stacks[s.tab].at(-1));
  return <div><p>Computer {app.profile.name}</p><output>{JSON.stringify(route)}</output>
    <button onClick={() => app.ui.push({ screen: "connections" }, "work")}>Connections</button>
    <button onClick={() => app.switchTo("sim-spare")}>Choose spare</button>
    <button onClick={() => app.switchTo(profile.profileId)}>Choose studio</button>
  </div>;
} }));

beforeEach(() => {
  localStorage.clear();
  prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  fake = platform();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

it.each([
  { sameComputer: false, taskHeld: true },
  { sameComputer: true, taskHeld: true },
  { sameComputer: false, taskHeld: false },
  { sameComputer: true, taskHeld: false },
])("Return routes and persists on the originating computer ($sameComputer current, $taskHeld task)", async ({ sameComputer, taskHeld }) => {
  const other = secondProfile(profile);
  fake.value.profiles = async () => [profile, other];
  const links: FakeLink[] = [];
  fake.value.control = id => {
    const link = new FakeLink();
    links.push(link);
    if (id === profile.profileId) {
      const view = fx.control(100).find(v => v.target_id === fx.ids.tgDp1)!;
      link.snap.control = [{ ...view,
        input_state: "paused", lease: null,
        model_observation: taskHeld ? view.model_observation : null,
        private_step: { requested_at: 100, ready: true, prior_observations_delivered: 1,
          input_stopped: "acknowledged", managed_screenshots_stopped: "acknowledged", uncovered_routes: [] },
      }];
    } else {
      link.snap.host = { ...link.snap.host, host_id: other.hostId, name: other.name };
      link.snap.tasks = []; link.snap.runs = []; link.snap.approvals = [];
    }
    return link;
  };
  const mounted = render(<App />);
  await screen.findByText("Computer studio");
  // Change the live store just before switching, including its delayed persistence.
  fireEvent.click(screen.getByRole("button", { name: "Connections" }));
  fireEvent.click(screen.getByRole("button", { name: "Choose spare" }));
  fireEvent.click(await screen.findByRole("button", { name: "Leave control and switch" }));
  await screen.findByText("Computer spare");
  if (sameComputer) {
    fireEvent.click(screen.getByRole("button", { name: "Choose studio" }));
    await screen.findByText("Computer studio");
    expect(screen.getByRole("status").textContent).toBe(JSON.stringify({ screen: "connections" }));
  }
  fireEvent.click(screen.getByText("Handoff details"));
  fireEvent.click(screen.getByRole("button", { name: "Return to studio" }));
  const destination = taskHeld ? { screen: "computer_use", taskId: fx.ids.tDash } : { screen: "desktop", targetId: fx.ids.tgDp1 };
  await screen.findByText("Computer studio");
  await waitFor(() => expect(screen.getByRole("status").textContent).toBe(JSON.stringify(destination)));
  // Unmount and pagehide both flush stores; neither can restore Connections.
  mounted.unmount();
  act(() => window.dispatchEvent(new Event("pagehide")));
  expect(new UiStore(profile.profileId).route()).toEqual(destination);
  expect(new UiStore(profile.profileId).get().tab).toBe("control");
  expect(links.flatMap(l => l.sent)).toEqual([{ command: "pause_input", target_id: fx.ids.tgDp1 }]);
});
