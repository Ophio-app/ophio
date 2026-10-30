// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../../app/App";
import { useApp, type App as AppValue } from "../../app/context";
import { prefs } from "../../app/prefs";
import { Toasts } from "../../design/Toast";
import { secondProfile } from "../../dev/sim/lifecycle";
import { platform, profile } from "../../platform/lifecycle.fixtures";
import { annotationStore } from "../../state/annotations";
import { removeMarkup } from "./removedMarkup";

let fake: ReturnType<typeof platform>;
let shown: AppValue;
vi.mock("../../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  shown = app;
  return <div><p>Computer {app.profile.name}</p>
    <button onClick={() => app.switchTo("sim-spare")}>Choose spare</button>
    <button onClick={() => app.switchTo(profile.profileId)}>Choose studio</button>
    <Toasts />
  </div>;
} }));

beforeEach(() => {
  localStorage.clear();
  prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  fake = platform();
  fake.value.profiles = async () => [profile, secondProfile(profile)];
});
afterEach(cleanup);

it("makes removed markup the message's again when it is changed after switching computers and back", async () => {
  render(<App />);
  await screen.findByText("Computer studio");
  const store = annotationStore(profile.profileId);
  const { id } = store.create("project", null);
  act(() => {
    shown.ui.saveDraft("project", null, { text: "Look at this", annotationId: id });
    shown.ui.push({ screen: "annotate", annotationId: id });
    shown.ui.push({ screen: "review", projectId: "project", taskId: null, intent: "new_task" });
    removeMarkup(shown.ui, store, "project", null, id);
    shown.ui.back();
  });
  // Undo is no longer offered, and Annotate still shows the markup.
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  fireEvent.click(screen.getByRole("button", { name: "Choose spare" }));
  await screen.findByText("Computer spare");
  fireEvent.click(screen.getByRole("button", { name: "Choose studio" }));
  await screen.findByText("Computer studio");

  act(() => { store.update(id, (d) => ({ ...d, generalNote: "Tighten the spacing" })); });

  expect(shown.ui.draft("project", null)?.annotationId).toBe(id);
  act(() => { shown.ui.back(); });
  expect(store.get()[id]?.generalNote).toBe("Tighten the spacing");
});

it("stops watching removed markup once its computer is forgotten", async () => {
  render(<App />);
  await screen.findByText("Computer studio");
  const store = annotationStore(profile.profileId);
  const ui = shown.ui;
  const watching = new Set<() => void>();
  for (const target of [store, ui]) {
    const subscribe = target.subscribe;
    vi.spyOn(target, "subscribe").mockImplementation((listener) => {
      const off = subscribe(listener);
      const end = () => { watching.delete(end); off(); };
      watching.add(end);
      return end;
    });
  }
  const { id } = store.create("project", null);
  act(() => {
    ui.saveDraft("project", null, { text: "Look at this", annotationId: id });
    ui.push({ screen: "annotate", annotationId: id });
    removeMarkup(ui, store, "project", null, id);
  });
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(watching.size).toBe(2);

  await act(() => shown.forget());
  await screen.findByText("Computer spare");

  expect(watching.size).toBe(0);
});
