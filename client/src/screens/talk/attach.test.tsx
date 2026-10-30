// SPDX-License-Identifier: Apache-2.0
// Attach in Chat: a capture, images, a clip or the markup, each opening the
// drawing at that step.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { annotationStore } from "../../state/annotations";
import { AttachSheet } from "./Attach";
import { Composer } from "./Composer";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

function composer() {
  const host = window.__sim!.host;
  const task = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const project = host.projects.find((p) => p.project_id === task.project_id)!;
  const harness = app.session.state.get().harnesses.find((h) => h.kind === task.harness)!;
  const route: Route = { screen: "talk" };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Composer project={project} task={task} run={null} harness={harness} agent={null} /></RouteProvider></AppProvider>);
  return task;
}
const sheet = () => within(screen.getByRole("dialog", { name: "Attach" }));
const opened = () => {
  const route = app.ui.route();
  if (route.screen !== "annotate") throw new Error(`Expected the drawing, not ${route.screen}`);
  return route.annotationId;
};

it("offers a capture, images, a clip and the markup in one place", () => {
  composer();
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  expect(sheet().getAllByRole("button").map((b) => b.querySelector(".row__title")?.textContent).filter(Boolean)).toEqual(["Screen", "Images", "Clip", "Mark up"]);
  expect(screen.queryByRole("button", { name: "Mark up a screenshot" })).toBeNull();
});

it("Screen opens a new drawing at the capture choices", () => {
  const task = composer();
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  fireEvent.click(sheet().getByRole("button", { name: /^Screen/ }));
  const id = opened();
  expect(app.ui.draft(task.project_id, task.task_id)?.annotationId).toBe(id);
  expect(app.ui.claimSource(id)).toEqual({ kind: "screen" });
});

it("images chosen under Attach go to the drawing to be added there", () => {
  composer();
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  const input = screen.getByLabelText("Images to attach") as HTMLInputElement;
  expect(input.multiple).toBe(true);
  const files = [new File(["a"], "a.png", { type: "image/png" }), new File(["b"], "b.png", { type: "image/png" })];
  Object.defineProperty(input, "files", { configurable: true, value: files });
  fireEvent.change(input);
  expect(app.ui.claimSource(opened())).toEqual({ kind: "files", files });
  expect(screen.queryByRole("dialog", { name: "Attach" })).toBeNull();
});

it("a clip is one file, and choosing nothing leaves the sheet open", () => {
  composer();
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  const input = screen.getByLabelText("Clip to attach") as HTMLInputElement;
  Object.defineProperty(input, "files", { configurable: true, value: [] });
  fireEvent.change(input);
  expect(app.ui.route().screen).toBe("talk");
  expect(screen.getByRole("dialog", { name: "Attach" })).toBeTruthy();
  const clips = [new File(["c"], "c.webm", { type: "video/webm" }), new File(["d"], "d.webm", { type: "video/webm" })];
  Object.defineProperty(input, "files", { configurable: true, value: clips });
  fireEvent.change(input);
  expect(app.ui.claimSource(opened())).toEqual({ kind: "files", files: [clips[0]] });
});

it("adds to the markup already started, and Mark up says what it holds", () => {
  const task = composer();
  const store = annotationStore(app.profile.profileId);
  const id = store.create(task.project_id, task.task_id).id;
  store.record(id, (d) => ({ ...d, sources: [{
    key: "S1", blobKey: "b", artifactId: null, sha256: "abc", byteSize: 1, mediaType: "image/png", width: 4, height: 3,
    durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 1, derivedFrom: null, derivation: null, included: true,
  }], selectedSource: "S1" }));
  act(() => app.ui.saveDraft(task.project_id, task.task_id, { annotationId: id }));
  // Something to send: Attach steps aside for Send and is under the tools.
  expect(screen.queryByRole("button", { name: "Attach" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Message tools" }));
  fireEvent.click(screen.getByRole("button", { name: /^Attach/ }));
  const markUp = sheet().getByRole("button", { name: /^Mark up/ });
  expect(markUp.textContent).toContain("1 image");
  fireEvent.click(sheet().getByRole("button", { name: /^Screen/ }));
  expect(opened()).toBe(id);
});

it("Screen waits for a connection; images and clips don't", () => {
  render(<AppProvider app={app}><AttachSheet open onClose={() => {}} online={false} computer="Workstation" markup={null} onStart={() => {}} /></AppProvider>);
  const screenRow = screen.getByRole("button", { name: /^Screen/ }) as HTMLButtonElement;
  expect(screenRow.disabled).toBe(true);
  expect(screenRow.textContent).toContain("Capture needs a connection");
  expect((screen.getByRole("button", { name: /^Images/ }) as HTMLButtonElement).disabled).toBe(false);
  expect((screen.getByRole("button", { name: /^Clip/ }) as HTMLButtonElement).disabled).toBe(false);
});
