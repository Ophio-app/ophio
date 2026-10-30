// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import Connections from "../settings/Connections";
import Previews from "./Previews";
import { controlPart } from "./parts";

let app: App;
const advance = (ms = 1000) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const OnControl = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "control", depth: 0, active: true, route: { screen: "control" } }}>{children}</RouteProvider>
  </AppProvider>
);

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = {
    platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  controlPart.set("screens");
  vi.clearAllTimers();
  vi.useRealTimers();
});

it("adds a preview from Control in place, without leaving for another screen", async () => {
  act(() => app.ui.selectTab("control"));
  render(<Previews previews={[]} />, { wrapper: OnControl });
  fireEvent.click(screen.getByRole("button", { name: "Add a preview" }));
  const form = within(screen.getByRole("dialog", { name: "Add preview" }));
  fireEvent.change(form.getByLabelText("Preview label"), { target: { value: "Dev server" } });
  fireEvent.change(form.getByLabelText("Preview URL"), { target: { value: "http://localhost:8000" } });
  fireEvent.click(form.getByRole("button", { name: "Add preview" }));
  await advance(2000);
  expect(window.__sim!.host.previews.map((p) => p.label)).toContain("Dev server");
  expect(screen.queryByRole("dialog", { name: "Add preview" })).toBeNull();
  expect(app.ui.route()).toEqual({ screen: "control" });
});

it("removes a preview from Control, saying its server keeps running", async () => {
  const host = window.__sim!.host;
  const project = Object.keys(app.session.state.get().projects)[0]!;
  void app.session.command({ command: "register_preview", project_id: project, label: "Dev server", url: "http://127.0.0.1:5173/" });
  await advance(2000);
  const added = host.previews.find((p) => p.label === "Dev server")!;
  render(<Previews previews={[added]} compact title={null} />, { wrapper: OnControl });
  fireEvent.click(screen.getByRole("button", { name: "Remove Dev server" }));
  const sheet = within(screen.getByRole("dialog", { name: "Remove Dev server?" }));
  expect(sheet.getByText(/Its server keeps running/)).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Remove Dev server" }));
  await advance(2000);
  expect(host.previews.some((p) => p.preview_id === added.preview_id)).toBe(false);
});

it("a preview draft survives a tap outside while typing, system Back and remount; Cancel clears it", async () => {
  const view = render(<Previews previews={[]} />, { wrapper: OnControl });
  fireEvent.click(screen.getByRole("button", { name: "Add a preview" }));
  fireEvent.change(screen.getByLabelText("Preview label"), { target: { value: "My page" } });
  const url = screen.getByLabelText("Preview URL");
  fireEvent.change(url, { target: { value: "http://localhost:3000/" } });
  url.focus();
  const dialog = screen.getByRole("dialog", { name: "Add preview" });
  fireEvent.pointerDown(dialog);
  fireEvent.click(dialog);
  expect(screen.getByRole("dialog", { name: "Add preview" })).toBeTruthy();
  // Android's Back, which asks the app directly.
  act(() => { window.systemBack!(); });
  await advance(50);
  expect(screen.queryByRole("dialog", { name: "Add preview" })).toBeNull();
  view.unmount();
  render(<Previews previews={[]} />, { wrapper: OnControl });
  fireEvent.click(screen.getByRole("button", { name: "Add a preview" }));
  expect((screen.getByLabelText("Preview label") as HTMLInputElement).value).toBe("My page");
  expect((screen.getByLabelText("Preview URL") as HTMLInputElement).value).toBe("http://localhost:3000/");
  fireEvent.click(screen.getByRole("button", { name: "Cancel and clear draft" }));
  fireEvent.click(screen.getByRole("button", { name: "Add a preview" }));
  expect((screen.getByLabelText("Preview label") as HTMLInputElement).value).toBe("");
  expect((screen.getByLabelText("Preview URL") as HTMLInputElement).value).toBe("");
});

it("a draft kept before previews moved to Control, for a project since removed, adds to the project shown", async () => {
  localStorage.setItem(`connections-form.${app.profile.profileId}`, JSON.stringify({
    name: "", code: "", project: "0192f7a1-0000-7000-8000-0000000001ff", label: "Dev server", url: "http://127.0.0.1:5173/",
  }));
  render(<Previews previews={[]} />, { wrapper: OnControl });
  fireEvent.click(screen.getByRole("button", { name: "Add a preview" }));
  const sheet = within(screen.getByRole("dialog", { name: "Add preview" }));
  const shown = (sheet.getByLabelText("Project") as HTMLSelectElement).value;
  expect(app.session.state.get().projects[shown]).toBeTruthy();
  expect((sheet.getByLabelText("Preview label") as HTMLInputElement).value).toBe("Dev server");
  fireEvent.click(sheet.getByRole("button", { name: "Add preview" }));
  await advance(1000);
  expect(window.__sim!.host.previews.some((p) => p.project_id === shown && p.label === "Dev server")).toBe(true);
});

it("Connections only points to Control's Previews, and opens it there", async () => {
  const route = { screen: "connections" } as const;
  render(
    <AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Connections route={route} /></RouteProvider></AppProvider>,
  );
  await advance(1000);
  expect(screen.queryByRole("button", { name: "Add preview" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open Previews in Control" }));
  expect(app.ui.get().tab).toBe("control");
  expect(app.ui.route()).toEqual({ screen: "control" });
  expect(controlPart.get()).toBe("previews");
});
