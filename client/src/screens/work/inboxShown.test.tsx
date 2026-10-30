// SPDX-License-Identifier: Apache-2.0
// The capture inbox keeps what agents showed in the project, however long ago
// their tasks ended, beside what was shared to this device.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { COLLABORATION_FEATURE } from "../../app/collaborationFeature";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { ArtifactMeta, PreviewEndpoint, Query } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Inbox from "./Inbox";
import { PROJECT_SHOWN_FEATURE } from "./useProjectShown";

vi.mock("./pictures", async (original) => ({
  ...(await original<typeof import("./pictures")>()),
  usePicture: (id: string | undefined) => ({ picture: id ? { url: `blob:${id}`, sha256: "s", width: 480, height: 270 } : undefined, error: undefined, retry() {} }),
}));

const AT = 1_800_000_000_000;
/** A task of tidewater-api that ended long ago, no longer in the phone's state. */
const ENDED = "0192f7a1-0000-7000-8000-0000000002ff";
const artifact = (artifact_id: string, task_id: string, created_at: number, role = "shown", media_type = "image/png"): ArtifactMeta => ({
  artifact_id, media_type, created_at, sha256: "a".repeat(64), size: 10, label: `${artifact_id}.png`, task_id,
  links: [{ task_id, run_id: null, annotation: null, role }],
} as unknown as ArtifactMeta);
const page = (preview_id: string, automatic: boolean, reachable: boolean): PreviewEndpoint => ({
  preview_id, project_id: ids.pTide, label: preview_id, url: "http://127.0.0.1:5179/", reachable, checked_at: AT, automatic,
  start: automatic ? null : "npm run dev",
} as unknown as PreviewEndpoint);

let app: App;
let asked: Query[];
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  window.__sim!.host.dispose();
  app.ui.selectProject(ids.pTide);
  asked = [];
  const query = session.query.bind(session);
  // The computer lists two pages: one from an ended task, then an older one.
  vi.spyOn(session, "query").mockImplementation((async (q: Query) => {
    if (q.kind !== "project_shown") return query(q);
    asked.push(q);
    return q.before
      ? { kind: "project_shown", history: { artifacts: [artifact("oldest", ENDED, AT)], next_before: null } }
      : { kind: "project_shown", history: { artifacts: [artifact("ended", ENDED, AT + 1000, "shown", "video/webm")], next_before: "ended" } };
  }) as never);
  act(() => session.state.set((s) => ({
    ...s,
    capabilities: s.capabilities && { ...s.capabilities, features: [...s.capabilities.features, COLLABORATION_FEATURE] },
    artifacts: {
      // Shown since in a running task.
      now: artifact("now", ids.tDash, AT + 2000),
      // A screenshot the agent took isn't something it showed.
      screenshot: artifact("screenshot", ids.tDash, AT + 3000, "observation"),
      // Another project's.
      notes: artifact("notes", ids.tNotes, AT + 4000),
    },
    previews: { ...s.previews, [ids.pTide]: [page("Checkout", false, false), page("Found site", true, true)] },
  })));
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const show = () => render(
  <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}><Inbox route={{ screen: "inbox" }} /></RouteProvider></AppProvider>,
);
const tiles = () => [...document.querySelectorAll(".talk-media__tile")].map((t) => t.getAttribute("aria-label"));

it("lists what agents showed in the project, from ended tasks too, and the pages to start again", async () => {
  show();
  await tick();
  expect(asked).toEqual([
    { kind: "project_shown", project_id: ids.pTide, before: null, limit: 100 },
    { kind: "project_shown", project_id: ids.pTide, before: "ended", limit: 100 },
  ]);
  expect(screen.getByRole("heading", { name: "From agents in tidewater-api" })).toBeTruthy();
  expect(tiles()).toEqual(["now.png", "ended.png", "oldest.png"]);
  // What agents added stays; a site found running is Control's.
  const pages = screen.getByRole("heading", { name: "Pages" }).parentElement!;
  expect(within(pages).getByRole("button", { name: "Open Checkout" })).toBeTruthy();
  expect(within(pages).queryByRole("button", { name: "Open Found site" })).toBeNull();
  expect(screen.getByRole("heading", { name: "Shared to this device" })).toBeTruthy();
});

it("opens one to draw on in the task it was shown in, or deletes it from the computer", async () => {
  show();
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "oldest.png" }));
  expect(screen.getByRole("button", { name: "Draw on it" })).toBeTruthy();
  const command = vi.spyOn(app.session, "command").mockResolvedValueOnce(null as never);
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(screen.getByText("Delete this picture from studio?")).toBeTruthy();
  fireEvent.click(screen.getAllByRole("button", { name: "Delete" }).at(-1)!);
  await tick();
  expect(command).toHaveBeenLastCalledWith({ command: "delete_artifact", artifact_id: "oldest" }, { label: "Delete picture" });
  // Gone from the computer, it is gone from the inbox.
  act(() => app.session.state.set((s) => {
    const { oldest: _, ...artifacts } = s.artifacts;
    return { ...s, artifacts };
  }));
  expect(tiles()).toEqual(["now.png", "ended.png"]);
});

it("says to update the computer when it can't list what ended tasks showed", async () => {
  act(() => app.session.state.set((s) => ({
    ...s,
    capabilities: s.capabilities && { ...s.capabilities, features: s.capabilities.features.filter((f) => f !== PROJECT_SHOWN_FEATURE) },
  })));
  show();
  await tick();
  expect(asked).toEqual([]);
  expect(screen.getByText(/Update Øphio on studio to list pictures and videos from tasks that have ended/)).toBeTruthy();
  // What running tasks showed is still there.
  expect(tiles()).toEqual(["now.png"]);
});
