// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { Toasts } from "../../design/Toast";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { annotationStore, blobs, type AnnotationDraft, type SourceDraft } from "../../state/annotations";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";

let app: App;
const removed: string[] = [];
const image = (key: string, draft: string): SourceDraft => ({
  key, blobKey: `${draft}-${key}`, artifactId: null, sha256: `sha-${key}`, mediaType: "image/png", width: 600, height: 400,
  durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 100, derivedFrom: null, derivation: null,
});
beforeEach(async () => {
  localStorage.clear(); removed.length = 0; vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(blobs, "remove").mockImplementation(async (key) => { removed.push(key); });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

/** A new-task composer holding text and two marked-up images. */
function composer() {
  const project = app.session.state.get().projects[ids.pTide]!;
  const store = annotationStore(app.profile.profileId);
  const { id } = store.create(project.project_id, null);
  store.update(id, (d) => ({ ...d, sources: [image("S1", id), image("S2", id)], nextSource: 3, generalNote: "Tighten the spacing" }));
  app.ui.saveDraft(project.project_id, null, { text: "Look at these two", annotationId: id });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Composer project={project} task={null} run={null} harness={null} agent={null} />
    <Toasts />
  </RouteProvider></AppProvider>);
  return { store, id, draft: () => app.ui.draft(project.project_id, null) };
}

it("offers Undo when the markup is removed, which puts back exactly the same draft", () => {
  const { store, id, draft } = composer();
  const before = store.get()[id];
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  expect(draft()?.annotationId).toBeNull();
  expect(screen.queryByRole("button", { name: "Remove the markup" })).toBeNull();
  expect(screen.getByText("Markup removed", { selector: ".toast__text" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(draft()).toMatchObject({ text: "Look at these two", annotationId: id });
  expect(store.get()[id]).toBe(before);
  expect(screen.getByRole("button", { name: "Remove the markup" })).toBeTruthy();
  expect(removed).toEqual([]);
});

it("deletes the markup and its images once Undo is no longer offered", async () => {
  const { store, id } = composer();
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  await act(async () => { await vi.advanceTimersByTimeAsync(7_000); });
  expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  expect(store.get()[id]).toBeUndefined();
  expect(removed.sort()).toEqual([`${id}-S1`, `${id}-S2`]);
});

it("keeps newer markup the message gained before Undo, and lets the removed one go", async () => {
  const { store, id, draft } = composer();
  const projectId = app.session.state.get().projects[ids.pTide]!.project_id;
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  const newer = store.create(projectId, null).id;
  store.update(newer, (d) => ({ ...d, sources: [image("S1", newer)], nextSource: 2 }));
  act(() => { app.ui.saveDraft(projectId, null, { annotationId: newer }); });
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(draft()?.annotationId).toBe(newer);
  expect(store.get()[newer]?.sources).toHaveLength(1);
  expect(screen.getByText("Markup not put back: the message has newer markup", { selector: ".toast__text" })).toBeTruthy();
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(store.get()[id]).toBeUndefined();
  expect(removed.sort()).toEqual([`${id}-S1`, `${id}-S2`]);
});

it("lets an empty markup that is not being edited give way to Undo, but not one open for editing", () => {
  const { store, id, draft } = composer();
  const projectId = app.session.state.get().projects[ids.pTide]!.project_id;
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  const empty = store.create(projectId, null).id;
  act(() => { app.ui.saveDraft(projectId, null, { annotationId: empty }); });
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(draft()?.annotationId).toBe(id);
  expect(store.get()[empty]).toBeUndefined();

  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  const open = store.create(projectId, null).id;
  act(() => { app.ui.saveDraft(projectId, null, { annotationId: open }); app.ui.push({ screen: "annotate", annotationId: open }); });
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(draft()?.annotationId).toBe(open);
  expect(store.get()[open]).toBeTruthy();
});

/** Removes the markup, gives the message newer markup filled by `fill`, then presses Undo. */
function undoOver(fill: (d: AnnotationDraft) => AnnotationDraft) {
  const { store, id, draft } = composer();
  const projectId = app.session.state.get().projects[ids.pTide]!.project_id;
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  const newer = store.create(projectId, null).id;
  store.update(newer, fill);
  const kept = store.get()[newer];
  act(() => { app.ui.saveDraft(projectId, null, { annotationId: newer }); });
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  return { store, id, newer, kept, draft };
}

it.each([
  ["words written for a comment but not yet added", (d: AnnotationDraft) => ({ ...d, composer: { text: "Make this blue", targets: [], general: true } })],
  ["a picture left out of the message", (d: AnnotationDraft) => ({ ...d, sources: [{ ...image("S1", d.id), included: false }], nextSource: 2 })],
  ["what was expected to happen", (d: AnnotationDraft) => ({ ...d, mode: "steps" as const, expected: "It saves" })],
])("keeps newer markup holding only %s", (_, fill) => {
  const { store, newer, kept, draft } = undoOver(fill);
  expect(draft()?.annotationId).toBe(newer);
  expect(store.get()[newer]).toBe(kept);
  expect(screen.getByText("Markup not put back: the message has newer markup", { selector: ".toast__text" })).toBeTruthy();
});

it("keeps removed markup while an Annotate screen still shows it, and lets it go once that closes", async () => {
  const { store, id, draft } = composer();
  act(() => { app.ui.push({ screen: "annotate", annotationId: id }); app.ui.push({ screen: "review", projectId: draft()!.projectId, taskId: null, intent: "new_task" }); });
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  // Back from Review to the Annotate screen beneath it, past the toast.
  act(() => { app.ui.back(); });
  await act(async () => { await vi.advanceTimersByTimeAsync(7_000); });
  expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  expect(store.get()[id]).toBeTruthy();
  expect(removed).toEqual([]);
  act(() => { app.ui.back(); });
  expect(store.get()[id]).toBeUndefined();
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(removed.sort()).toEqual([`${id}-S1`, `${id}-S2`]);
});

it("makes removed markup the message's again when it is changed on the Annotate screen still showing it", async () => {
  const { store, id, draft } = composer();
  act(() => { app.ui.push({ screen: "annotate", annotationId: id }); app.ui.push({ screen: "review", projectId: draft()!.projectId, taskId: null, intent: "new_task" }); });
  fireEvent.click(screen.getByRole("button", { name: "Remove the markup" }));
  act(() => { app.ui.back(); });
  await act(async () => { await vi.advanceTimersByTimeAsync(7_000); });
  // Still editing after Undo is gone.
  act(() => { store.update(id, (d) => ({ ...d, generalNote: "Tighten the spacing and the margins" })); });
  expect(draft()?.annotationId).toBe(id);
  act(() => { app.ui.back(); });
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(store.get()[id]?.generalNote).toBe("Tighten the spacing and the margins");
  expect(removed).toEqual([]);
});
