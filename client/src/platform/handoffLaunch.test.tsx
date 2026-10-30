// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp, useLink } from "../app/context";
import { prefs } from "../app/prefs";
import { FakeLink, platform, profile } from "./lifecycle.fixtures";
import * as fx from "../dev/sim/scenario";
import { secondProfile } from "../dev/sim/lifecycle";

/** One control link per computer at a time, as src-tauri/src/links.rs
 * (Links::open) allows: a second one is refused while the first is open. */
function nativeLikeControl(opened: string[] = []) {
  const open = new Map<string, FakeLink>();
  return (id: string) => {
    opened.push(id);
    const link = new FakeLink();
    if (id !== profile.profileId) link.snap.host = { ...link.snap.host, host_id: secondProfile(profile).hostId, name: "spare" };
    const first = open.get(id);
    if (first && !first.stopped) link.offline = true;
    else open.set(id, link);
    return link;
  };
}
const kept = () => JSON.stringify([{ profileId: profile.profileId, since: 1, busy: false, detail: "",
  risk: { leases: [{ leaseId: fx.ids.lease1, targetId: fx.ids.tgDp1 }], privateTargets: [], stoppingTasks: [], pendingInput: [] } }]);

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const link = useLink();
  return <div><p>Computer {app.profile.name}</p><p>Link {link.state}</p>
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

it("after a restart, a kept handoff for the computer in use shares its one connection and clears once control has ended", async () => {
  // The app was closed while this computer's control handoff was unconfirmed,
  // with this computer in use (the person had returned to it).
  localStorage.setItem("unresolved-computers.v1", kept());
  const opened: string[] = [];
  fake.value.control = nativeLikeControl(opened);
  render(<App />);
  await screen.findByText("Computer studio");
  // The computer reports no control held by this device.
  await waitFor(() => expect(JSON.parse(localStorage.getItem("unresolved-computers.v1") ?? "null")).toEqual([]));
  expect(opened.filter((id) => id === profile.profileId)).toHaveLength(1);
});

it("after a restart with a kept handoff for the computer in use, returning to it later connects", async () => {
  localStorage.setItem("unresolved-computers.v1", kept());
  fake.value.profiles = async () => [profile, secondProfile(profile)];
  fake.value.control = nativeLikeControl();
  render(<App />);
  await screen.findByText("Link online");
  fireEvent.click(screen.getByRole("button", { name: "Choose spare" }));
  await screen.findByText("Computer spare");
  fireEvent.click(screen.getByRole("button", { name: "Choose studio" }));
  await screen.findByText("Computer studio");
  await screen.findByText("Link online");
});
