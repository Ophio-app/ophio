// SPDX-License-Identifier: Apache-2.0
// Markup removed from a message and still open in Annotate outlives a restart
// of the app: changed there, it becomes the message's markup again; left
// unchanged, it is deleted once Annotate closes.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import type { App as AppValue } from "../../app/context";
import type { UiStore } from "../../app/ui";
import { platform, profile } from "../../platform/lifecycle.fixtures";

let fake: ReturnType<typeof platform>;
let shown: AppValue;
// Each interface record the app has kept, stopped after the test so that it
// writes nothing into the next.
const kept = new Set<UiStore>();
// The modules of the app now running: the mocked shell outlives a restart.
let running: { useApp: () => AppValue; Toasts: () => ReactNode };
vi.mock("../../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../../shell/Shell", () => ({ Shell: function TestShell() {
  const app = running.useApp();
  shown = app;
  kept.add(app.ui);
  return <div><p>Computer {app.profile.name}</p><running.Toasts /></div>;
} }));

beforeEach(() => {
  localStorage.clear();
  vi.resetModules();
  fake = platform();
});
afterEach(() => {
  cleanup();
  for (const ui of kept) ui.forget();
  kept.clear();
});

/** Starts the app afresh, as after the device closed it. */
async function start() {
  const [{ App }, { prefs }, { annotationStore }, { useApp }, { Toasts }] = await Promise.all([
    import("../../app/App"), import("../../app/prefs"), import("../../state/annotations"),
    import("../../app/context"), import("../../design/Toast"),
  ]);
  running = { useApp, Toasts };
  prefs.set({ ...prefs.get(), lastProfileId: profile.profileId });
  render(<App />);
  await screen.findByText("Computer studio");
  return annotationStore(profile.profileId);
}

/** Removes markup open in Annotate, lets Undo go and closes the app. */
async function removedThenRestarted(): Promise<string> {
  const store = await start();
  const { removeMarkup } = await import("./removedMarkup");
  const { id } = store.create("project", null);
  act(() => {
    shown.ui.saveDraft("project", null, { text: "Look at this", annotationId: id });
    shown.ui.push({ screen: "annotate", annotationId: id });
    shown.ui.push({ screen: "review", projectId: "project", taskId: null, intent: "new_task" });
    removeMarkup(shown.ui, store, "project", null, id);
    shown.ui.back();
  });
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  window.dispatchEvent(new Event("pagehide"));
  cleanup();
  vi.resetModules();
  return id;
}

it("makes removed markup the message's again when it is changed after a restart", async () => {
  const id = await removedThenRestarted();
  const store = await start();
  expect(shown.ui.route()).toMatchObject({ screen: "annotate", annotationId: id });
  expect(store.get()[id]).toBeDefined();
  expect(shown.ui.draft("project", null)?.annotationId ?? null).toBeNull();

  act(() => { store.update(id, (d) => ({ ...d, generalNote: "Tighten the spacing" })); });

  expect(shown.ui.draft("project", null)?.annotationId).toBe(id);
  act(() => { shown.ui.back(); });
  expect(store.get()[id]?.generalNote).toBe("Tighten the spacing");
  expect(shown.ui.get().removedMarkup).toEqual({});
});

it("deletes removed markup left unchanged once Annotate closes after a restart", async () => {
  const id = await removedThenRestarted();
  const store = await start();
  expect(store.get()[id]).toBeDefined();

  act(() => { shown.ui.back(); });

  expect(store.get()[id]).toBeUndefined();
  expect(shown.ui.draft("project", null)?.annotationId ?? null).toBeNull();
  expect(shown.ui.get().removedMarkup).toEqual({});
});
