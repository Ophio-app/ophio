// SPDX-License-Identifier: Apache-2.0
// A request that never reached the agent has one name for going back to the
// composer, in the receipt as in Talk, and a composer that already holds
// exactly those words and markup is opened as it is, with nothing to choose.
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { AnnotationBundle, Command, TimelineBody } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { annotationStore } from "../../state/annotations";
import Receipt from "./Receipt";
import { retryReceipt } from "./retryReceipt";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const send = (body: Command) => window.__sim!.host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null, body });
function view(route: Route & { screen: "receipt" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 2, active: true, route }}><Receipt route={route} /></RouteProvider></AppProvider>);
}
const words = "Also check the p99 line.";
const row = () => [...document.querySelectorAll<HTMLElement>(".receipt-request")].find(r => r.textContent?.includes(words))!;

it("the receipt puts a message withdrawn by Stop back in the composer, then says it is there", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  await send({ command: "send_message", task_id: task.task_id, text: words, bundle: null, delivery: "next_turn" });
  await send({ command: "stop_task", task_id: task.task_id });
  await tick(1500);
  view({ screen: "receipt", taskId: task.task_id });
  await tick(900);
  expect(within(row()).getByText("Withdrawn by Stop · not sent")).toBeTruthy();
  expect(within(row()).queryByRole("button", { name: "Send again" })).toBeNull();
  const commands = Object.keys(app.session.commands.get()).length;
  await act(async () => fireEvent.click(within(row()).getByRole("button", { name: "Put back in composer" })));
  expect(app.ui.draft(task.project_id, task.task_id)).toMatchObject({ text: words, annotationId: null });
  expect(Object.keys(app.session.commands.get())).toHaveLength(commands);
  expect(app.ui.get().tab).toBe("talk");
  // Talk's note, word for word, in place of a second offer.
  expect(within(row()).getByText("Back in the composer")).toBeTruthy();
  expect(within(row()).queryByRole("button", { name: "Put back in composer" })).toBeNull();
  act(() => app.ui.saveDraft(task.project_id, task.task_id, { text: `${words} And p50.` }));
  expect(within(row()).queryByText("Back in the composer")).toBeNull();
  expect(within(row()).getByRole("button", { name: "Put back in composer" })).toBeTruthy();
});

it("the receipt says a message withdrawn when its chat lost access wasn’t sent, and offers no resend", async () => {
  const host = window.__sim!.host;
  const task = app.session.state.get().tasks[ids.tDash]!;
  // As the computer records a chat's follow-up held for the next turn, then
  // its withdrawal when that chat is revoked.
  const append = (item: TimelineBody) => host.emit({ type: "timeline_appended", task_id: task.task_id,
    item: { seq: host.seq + 1, at: Date.now(), run_id: task.current_run, item } });
  const held = append({ kind: "user_message", text: words, bundle: null, delivery: "queued_for_boundary" });
  append({ kind: "delivery", message_seq: held, state: "cancelled", withdrawn_by_revocation: true });
  await tick();
  view({ screen: "receipt", taskId: task.task_id });
  await tick(900);
  expect(within(row()).getByText("Withdrawn when its chat lost access · not sent")).toBeTruthy();
  expect(within(row()).queryByRole("button", { name: "Put back in composer" })).toBeNull();
});

const bundle: AnnotationBundle = { bundle_id: "b1", revision: 1, marks: [], comments: [], steps: [], general_note: "Only the legend", speech: null,
  sources: [{ key: "S1", artifact_id: "art-1", sha256: "0".repeat(64), media_type: "image/png", width: 390, height: 200, captured_at: 1000, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: "c", host_id: "h", computer_name: "studio", preview_id: "notes", preview_label: "Notes", captured_at: 1000 } }] };

it("a composer that already holds the same words and markup opens as it is, with no draft to choose", () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const request = { item: { kind: "user_message" as const, text: words, bundle, delivery: "failed" as const } };
  retryReceipt(app, task, request);
  const first = app.ui.draft(task.project_id, task.task_id)!;
  app.ui.reset("work", [{ screen: "receipt", taskId: task.task_id }]);
  // The same request again: no conflict, no second copy of the markup, and Talk opens on it.
  const markups = Object.keys(annotationStore(app.profile.profileId).get()).length;
  retryReceipt(app, task, request);
  expect(app.ui.draft(task.project_id, task.task_id)).toBe(first);
  expect(Object.keys(annotationStore(app.profile.profileId).get())).toHaveLength(markups);
  expect(app.ui.get().savedDrafts).toEqual({});
  expect(app.ui.get().tab).toBe("talk");
  // The same words with the markup changed are a different draft, so the person chooses.
  annotationStore(app.profile.profileId).update(first.annotationId!, d => ({ ...d, generalNote: "The legend and the axis" }));
  expect(() => retryReceipt(app, task, request)).toThrow("There’s already a draft for this task.");
  // So are the same words without the markup.
  app.ui.saveDraft(task.project_id, task.task_id, { annotationId: null });
  expect(() => retryReceipt(app, task, request)).toThrow("There’s already a draft for this task.");
});
