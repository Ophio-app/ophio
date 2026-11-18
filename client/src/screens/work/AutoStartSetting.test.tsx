// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { AutoStart } from "../../protocol/types";
import { AutoStartSettingRow } from "./AutoStartSetting";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const host = () => window.__sim!.host;
const project = () => host().projects.find((p) => p.project_id === ids.pTide)!;
function setAutoStart(value?: AutoStart) {
  const next = { ...project(), auto_start: value };
  host().projects = host().projects.map((p) => p.project_id === ids.pTide ? next : p);
  host().emit({ type: "project_upserted", project: next });
}
function renderRow() {
  return render(<AppProvider app={app}><AutoStartSettingRow project={project()} /><Toasts /></AppProvider>);
}
function open() {
  fireEvent.click(screen.getByRole("button", { name: /Start what the agent proposes without asking/ }));
  return within(screen.getByRole("dialog", { name: "Start what the agent proposes without asking" }));
}
function sent(command: string) {
  return vi.mocked(app.session.command).mock.calls.map(([c]) => c).filter((c) => c.command === command);
}

beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  host().host.capabilities.features = [...host().host.capabilities.features, "chat_work_sets_v1"];
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  vi.spyOn(session, "command");
});
afterEach(() => {
  cleanup(); app.session.stop(); host().dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("is hidden when the computer does not offer chat work sets", async () => {
  const next = { ...app.session.state.get().capabilities!, features: [] };
  host().host.capabilities = next;
  host().emit({ type: "capabilities_changed", capabilities: next });
  await tick(100);
  renderRow();
  expect(screen.queryByRole("button", { name: /Start what the agent proposes/ })).toBeNull();
});

it("is hidden without the approvals grant", async () => {
  window.__sim!.permissions({ approvals: false });
  await tick(1500);
  renderRow();
  expect(screen.queryByRole("button", { name: /Start what the agent proposes/ })).toBeNull();
});

it("starts off and waits for the computer before showing on", async () => {
  renderRow();
  expect(screen.getByRole("button", { name: /Start what the agent proposes/ }).textContent).toContain("Off");
  const dialog = open();
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  // Delay the command so the setting cannot appear on merely because it was tapped.
  const original = app.session.command.bind(app.session);
  vi.mocked(app.session.command).mockImplementationOnce(async (...args) => {
    await new Promise<void>((resolve) => setTimeout(resolve, 700));
    return original(...args);
  });
  const emit = host().emit.bind(host());
  let held: Parameters<typeof emit> | null = null;
  vi.spyOn(host(), "emit").mockImplementation((...args) => {
    if (args[0].type === "project_upserted" && args[0].project.project_id === ids.pTide) {
      held = args;
      return 0;
    }
    return emit(...args);
  });
  fireEvent.click(dialog.getByRole("switch"));
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  expect((dialog.getByRole("switch") as HTMLInputElement).disabled).toBe(true);
  expect(dialog.getByRole("status").textContent).toBe("Turning on…");
  expect(sent("set_auto_start")).toMatchObject([{ project_id: ids.pTide, auto_start: { per_reply: 4, at_once: 4 } }]);
  await tick(1500);
  expect(held).not.toBeNull();
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  act(() => { emit(...held!); });
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(true);
  expect(screen.getByRole("button", { name: /Start what the agent proposes/ }).textContent).toContain("On, up to 4 at once");
});

it("says when work starts by itself, where, and that it stops at the pull request", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  const text = dialog.getByRole("switch").closest(".auto-start-setting")!.textContent!;
  expect(dialog.getByRole("switch", { name: "Allow in this project" })).toBeTruthy();
  expect(text).toContain("while answering your message starts without your tap");
  expect(text).toContain("Only from Work chats with full access");
  expect(text).toContain("never while Next up is paused");
  expect(text).toContain("stop it from its card");
  expect(text).toContain("stops at its pull request");
  expect(dialog.getAllByText(/Work chats with full access/)).toHaveLength(1);
  expect(dialog.getByRole("group", { name: "Tasks one reply may start" })).toBeTruthy();
});

it("turns off immediately and sends null", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  fireEvent.click(dialog.getByRole("switch"));
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  expect(sent("set_auto_start")).toMatchObject([{ project_id: ids.pTide, auto_start: null }]);
  await tick(1500);
});

it("sends a changed limit and waits to display an increase", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  const original = app.session.command.bind(app.session);
  vi.mocked(app.session.command).mockImplementationOnce(async (...args) => {
    await new Promise<void>((resolve) => setTimeout(resolve, 700));
    return original(...args);
  });
  fireEvent.click(dialog.getByRole("button", { name: "More running at once" }));
  expect(sent("set_auto_start")).toMatchObject([{ auto_start: { per_reply: 4, at_once: 5 } }]);
  expect(within(dialog.getByRole("group", { name: "Running at once" })).getByText("4")).toBeTruthy();
  await tick(1500);
  expect(within(dialog.getByRole("group", { name: "Running at once" })).getByText("5")).toBeTruthy();
});

it("restores a refused optimistic change and explains it in a toast", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  vi.mocked(app.session.command).mockRejectedValueOnce(new Error("The computer refused this change"));
  fireEvent.click(dialog.getByRole("switch"));
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  await tick(100);
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(true);
  expect(screen.getByText(/The computer refused this change/)).toBeTruthy();
});

/** The sheet's "Saving…" line; the counts are live outputs too. */
const saving = (dialog: ReturnType<typeof within>) => dialog.queryAllByRole("status").find((e: HTMLElement) => e.tagName === "P");

/** Holds the computer's own project update for this phone's change, so
 * another device's change can land first. Returns a way to emit as the
 * computer would, unheld. */
function holdOwnUpdate() {
  const emit = host().emit.bind(host());
  vi.spyOn(host(), "emit").mockImplementation((...args) => args[0].type === "project_upserted" && args[0].project.project_id === ids.pTide ? 0 : emit(...args));
  return (value: AutoStart | undefined) => act(() => {
    const next = { ...project(), auto_start: value };
    host().projects = host().projects.map((p) => p.project_id === ids.pTide ? next : p);
    emit({ type: "project_upserted", project: next });
  });
}

it("lets a later change from another device win once this phone's change is accepted", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  const other = holdOwnUpdate();
  fireEvent.click(dialog.getByRole("button", { name: "More running at once" }));
  await tick(1500);
  // Accepted, and still waiting for the computer's value.
  expect(saving(dialog)?.textContent).toBe("Saving…");
  other({ per_reply: 2, at_once: 3 });
  expect(saving(dialog)).toBeUndefined();
  expect((dialog.getByRole("switch") as HTMLInputElement).disabled).toBe(false);
  expect(within(dialog.getByRole("group", { name: "Running at once" })).getByText("3")).toBeTruthy();
  expect(within(dialog.getByRole("group", { name: "Tasks one reply may start" })).getByText("2")).toBeTruthy();
});

it("lets another device turning it off or on win over this phone's accepted change", async () => {
  setAutoStart({ per_reply: 4, at_once: 4 });
  await tick(100);
  renderRow();
  const dialog = open();
  let other = holdOwnUpdate();
  fireEvent.click(dialog.getByRole("button", { name: "More running at once" }));
  await tick(1500);
  other(undefined);
  expect(saving(dialog)).toBeUndefined();
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(false);
  expect((dialog.getByRole("switch") as HTMLInputElement).disabled).toBe(false);
  vi.mocked(host().emit).mockRestore();
  // Off here, turned on elsewhere: this phone's turning on gives way to it.
  other = holdOwnUpdate();
  fireEvent.click(dialog.getByRole("switch"));
  await tick(1500);
  expect(saving(dialog)?.textContent).toBe("Turning on…");
  other({ per_reply: 1, at_once: 2 });
  expect(saving(dialog)).toBeUndefined();
  expect((dialog.getByRole("switch") as HTMLInputElement).checked).toBe(true);
  expect(within(dialog.getByRole("group", { name: "Running at once" })).getByText("2")).toBeTruthy();
});
