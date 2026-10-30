// SPDX-License-Identifier: Apache-2.0
// What an agent shows through Ophio appears in Talk with the person's way
// to answer it, and the task's pictures and videos are kept in one place.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { COLLABORATION_FEATURE } from "../../app/collaborationFeature";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { CommandError, HostSession } from "../../host/session";
import type { ArtifactMeta, Shown } from "../../protocol/types";
import { annotationStore, blobs } from "../../state/annotations";
import { RouteProvider } from "../../shell/Screen";
import { rows } from "./timeline";
import { MediaSheet } from "./RecentMedia";
import { ShownCard } from "./Shown";
import { transferComposer } from "../transfers/composer";

const bytes = (text: string, type: string) =>
  Object.assign(new Blob([text], { type }), { arrayBuffer: async () => new TextEncoder().encode(text).buffer });
const picture = bytes("checkout picture", "image/png");

const loaded: (string | undefined)[] = [];
const expired = new Set<string>();
const EXPIRED = "Removed from the computer after a week unopened";
vi.mock("../work/pictures", () => ({
  usePicture: (id: string | undefined) => (loaded.push(id), id && expired.has(id) ? { error: EXPIRED, gone: true, retry() {} } : {
    picture: id ? { url: `blob:${id}`, blob: picture, sha256: "s", width: 480, height: 270 } : undefined,
    error: undefined,
    retry() {},
  }),
  acquirePicture: () => ({ promise: Promise.resolve({ url: "blob:x", blob: picture, sha256: "s", width: 480, height: 270 }), release() {} }),
}));
vi.mock("../work/useTaskArtifacts", () => ({ useTaskArtifacts: () => ({ loading: false, error: null, missingGrants: [] }) }));

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("createImageBitmap", async () => ({ width: 480, height: 270, close() {} }));
  const stored = new Map<string, Blob>();
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => { const key = `blob-${stored.size}`; stored.set(key, blob); return key; });
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  window.__sim!.host.dispose();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const show = (node: React.ReactNode) => render(
  <AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>{node}</RouteProvider></AppProvider>,
);
const AT = 1_800_000_000_000;
const media = (artifact_id: string, media_type: string, invite: "draw" | "talk" | null): Shown =>
  ({ what: "media", artifact_id, media_type, path: `shots/${artifact_id}.${media_type.split("/")[1]}`, caption: "Here it is now.", invite });
const artifact = (artifact_id: string, media_type: string, created_at: number, role: "observation" | "source" | "shown"): ArtifactMeta => ({
  artifact_id, media_type, created_at, sha256: "a".repeat(64), size: 10, label: `${artifact_id}.png`, task_id: ids.tDash,
  links: [{ task_id: ids.tDash, run_id: null, annotation: null, role }],
} as unknown as ArtifactMeta);

it("lists what the agent showed as a row of the conversation", () => {
  const shown = media("pic", "image/png", "draw");
  expect(rows([{ seq: 1, at: AT, run_id: null, item: { kind: "shown", shown } }])).toEqual([
    { kind: "shown", key: expect.any(String), at: AT, shown },
  ]);
});

it("opens a picture the agent showed in the task's drawing, as its own source", async () => {
  show(<ShownCard shown={media("pic", "image/png", "draw")} taskId={ids.tDash} at={AT} />);
  expect(screen.getByText("Here it is now.")).toBeTruthy();
  expect(screen.getByText(/Picture from the agent · pic\.png/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Draw on it" }));
  // Hashing the picture takes real time.
  await act(() => vi.waitFor(() => expect(app.ui.route()).toMatchObject({ screen: "annotate" })));
  const route = app.ui.route();
  const id = route.screen === "annotate" ? route.annotationId : "";
  const draft = annotationStore(app.profile.profileId).get()[id]!;
  expect(draft.sources).toHaveLength(1);
  expect(draft.sources[0]).toMatchObject({ artifactId: "pic", origin: { kind: "shown", artifact_id: "pic" } });
  expect(draft.selectedSource).toBe(draft.sources[0]!.key);
  expect(app.ui.draft(ids.pTide, ids.tDash)?.annotationId).toBe(id);

  // Answering it again goes back to the same picture rather than a copy.
  act(() => app.ui.back());
  fireEvent.click(screen.getByRole("button", { name: "Draw on it" }));
  await act(() => vi.waitFor(() => expect(app.ui.route()).toMatchObject({ screen: "annotate" })));
  expect(annotationStore(app.profile.profileId).get()[id]!.sources).toHaveLength(1);
});

it("waits to load a video until the person plays it, and answers it by talking", () => {
  loaded.length = 0;
  show(<ShownCard shown={media("clip", "video/webm", "talk")} taskId={ids.tDash} at={AT} />);
  expect(document.querySelector("video")).toBeNull();
  expect(loaded.filter(Boolean)).toEqual([]);
  fireEvent.click(screen.getByRole("button", { name: "Watch clip.webm" }));
  expect(document.querySelector("video")?.getAttribute("src")).toBe("blob:clip");
  expect(screen.getByRole("button", { name: "Talk over it" })).toBeTruthy();
});

it("says when a picture the agent showed has expired, with nothing left to do over it", () => {
  expired.add("old");
  show(<ShownCard shown={media("old", "image/png", "draw")} taskId={ids.tDash} at={AT} />);
  expect(screen.getByText(EXPIRED)).toBeTruthy();
  expect(screen.getByText(/Picture from the agent · old\.png/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Draw on it" })).toBeNull();
});

it("starts the drawing at the capture choices when the agent asks for a capture", () => {
  show(<ShownCard shown={{ what: "invitation", action: "capture", note: "Could you capture the page?" }} taskId={ids.tDash} at={AT} />);
  fireEvent.click(screen.getByRole("button", { name: "Capture the screen" }));
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "annotate" });
  const id = route.screen === "annotate" ? route.annotationId : "";
  expect(annotationStore(app.profile.profileId).get()[id]).toMatchObject({ projectId: ids.pTide, taskId: ids.tDash });
  expect(app.ui.claimSource("another")).toBeNull();
  expect(app.ui.claimSource(id)).toEqual({ kind: "screen" });
  expect(app.ui.claimSource(id)).toBeNull();
});

it("leaves a pending message's pictures alone when the agent asks for a capture", () => {
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: "Sent?", attempt: { id: "a", state: "unknown" } as never });
  show(<ShownCard shown={{ what: "invitation", action: "capture", note: "Could you capture the page?" }} taskId={ids.tDash} at={AT} />);
  fireEvent.click(screen.getByRole("button", { name: "Capture the screen" }));
  expect(app.ui.route()).toMatchObject({ screen: "talk" });
  expect(screen.getByText(/Check the task’s pending message/)).toBeTruthy();
});

it("takes the person to the computer or to sending files when the agent asks", () => {
  act(() => app.session.state.set((s) => ({ ...s, targets: [{ target_id: "desk", kind: "desktop" } as never] })));
  show(<>
    <ShownCard shown={{ what: "invitation", action: "watch_desktop", note: "Watch me try it." }} taskId={ids.tDash} at={AT} />
    <ShownCard shown={{ what: "invitation", action: "send_files", note: "Send me the logo." }} taskId={ids.tDash} at={AT} />
  </>);
  fireEvent.click(screen.getByRole("button", { name: "Watch the computer" }));
  expect(app.ui.route()).toEqual({ screen: "desktop", targetId: "desk" });
  fireEvent.click(screen.getByRole("button", { name: "Send files" }));
  expect(app.ui.route()).toEqual({ screen: "transfers" });
  expect(transferComposer(app.profile.profileId).get().destination).toEqual({ kind: "project_dir", project_id: ids.pTide, dir: "" });
});

it("sends a task in Home the files it asks for through the Inbox, since Home takes none", () => {
  act(() => app.session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, project_id: ids.pHome } } })));
  show(<ShownCard shown={{ what: "invitation", action: "send_files", note: "Send me the logo." }} taskId={ids.tDash} at={AT} />);
  fireEvent.click(screen.getByRole("button", { name: "Send files" }));
  expect(transferComposer(app.profile.profileId).get().destination).toEqual({ kind: "inbox" });
});

it("starts computer use with the agent's goal, or voice control, when the agent suggests it", () => {
  const key = `computer-goal.${app.profile.profileId}.${ids.pTide}`;
  localStorage.removeItem(key);
  show(<>
    <ShownCard shown={{ what: "invitation", action: "computer_use", note: "Try it as a user would.", goal: "Sign up in the app" }} taskId={ids.tDash} at={AT} />
    <ShownCard shown={{ what: "invitation", action: "voice_control", note: "Walk through it by voice." }} taskId={ids.tDash} at={AT} />
  </>);
  expect(screen.getByText("Goal: Sign up in the app")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start computer use" }));
  expect(app.ui.route()).toEqual({ screen: "computer_use_review", projectId: ids.pTide });
  expect(JSON.parse(localStorage.getItem(key) ?? "{}")).toMatchObject({ goal: "Sign up in the app" });
  // A goal the person wrote is theirs to keep.
  localStorage.setItem(key, JSON.stringify({ goal: "My own goal", target: "", harness: "", model: "" }));
  fireEvent.click(screen.getByRole("button", { name: "Start computer use" }));
  expect(JSON.parse(localStorage.getItem(key) ?? "{}")).toMatchObject({ goal: "My own goal" });
  localStorage.removeItem(key);
  fireEvent.click(screen.getByRole("button", { name: "Control by voice" }));
  expect(app.ui.route()).toEqual({ screen: "voice" });
});

it("shows the preview the agent started, and says when it is gone", () => {
  const preview = { preview_id: "pv", project_id: ids.pTide, label: "Checkout", url: "http://127.0.0.1:5173", reachable: true, checked_at: AT };
  act(() => app.session.state.set((s) => ({ ...s, previews: { ...s.previews, [ids.pTide]: [preview as never] } })));
  const card = <ShownCard shown={{ what: "preview", preview_id: "pv", label: "Checkout", url: preview.url, note: "Try it." }} taskId={ids.tDash} at={AT} />;
  const { rerender } = show(card);
  expect(screen.getByText(/Preview from the agent · Checkout/)).toBeTruthy();
  expect(screen.queryByText(/no longer one of this project’s previews/)).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, previews: { ...s.previews, [ids.pTide]: [] } })));
  rerender(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>{card}</RouteProvider></AppProvider>);
  expect(screen.getByText("http://127.0.0.1:5173 is no longer one of this project’s previews.")).toBeTruthy();
  // Its server stopped a while and started again: the computer found it anew.
  const found = { ...preview, preview_id: "found", label: "Shop", url: "http://localhost:5173/", automatic: true };
  act(() => app.session.state.set((s) => ({ ...s, previews: { ...s.previews, [ids.pTide]: [found as never] } })));
  rerender(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>{card}</RouteProvider></AppProvider>);
  expect(screen.queryByText(/no longer one of this project’s previews/)).toBeNull();
  expect(screen.getByRole("button", { name: /Open\s*Shop/ })).toBeTruthy();
});

it("keeps the task's pictures and videos newest first, and deletes only what wasn't sent", async () => {
  act(() => app.session.state.set((s) => ({
    ...s,
    capabilities: s.capabilities && { ...s.capabilities, features: [...s.capabilities.features, COLLABORATION_FEATURE] },
    artifacts: {
      ...s.artifacts,
      old: artifact("old", "image/png", AT, "source"),
      shot: artifact("shot", "image/png", AT + 1000, "observation"),
      // Shown in a part of the conversation not loaded: what it is comes with it.
      clip: artifact("clip", "video/webm", AT + 2000, "shown"),
    },
  })));
  show(<MediaSheet open onClose={() => {}} taskId={ids.tDash} projectId={ids.pTide} />);
  const tiles = () => [...document.querySelectorAll(".talk-media__tile")].map((t) => t.getAttribute("aria-label"));
  expect(tiles().slice(0, 3)).toEqual(["From the agent: clip.png", "Captured: shot.png", "You sent: old.png"]);
  const tags = [...document.querySelectorAll(".talk-media__tag")].map((t) => t.textContent);
  expect(tags.slice(0, 3)).toEqual(["Agent", "Captured", "You sent"]);

  // What the person sent stays with their message.
  fireEvent.click(screen.getByRole("button", { name: "You sent: old.png" }));
  expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Draw on it" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "All pictures and videos" }));

  fireEvent.click(screen.getByRole("button", { name: "From the agent: clip.png" }));
  expect(screen.getByRole("button", { name: "Talk over it" })).toBeTruthy();
  const command = vi.spyOn(app.session, "command")
    .mockRejectedValueOnce(new CommandError({ code: "conflict", message: "This picture is part of a message to an agent, so it stays with that message" } as never, null))
    .mockResolvedValueOnce(null as never);
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(screen.getByText("Delete this video from studio?")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Keep" }));
  expect(command).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  fireEvent.click(screen.getAllByRole("button", { name: "Delete" }).at(-1)!);
  await tick();
  expect(command).toHaveBeenLastCalledWith({ command: "delete_artifact", artifact_id: "clip" }, { label: "Delete picture" });
  expect(screen.getByText(/stays with that message/)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  fireEvent.click(screen.getAllByRole("button", { name: "Delete" }).at(-1)!);
  await tick();
  expect(command).toHaveBeenCalledTimes(2);
  expect(document.querySelector(".talk-media__grid")).toBeTruthy();
});
