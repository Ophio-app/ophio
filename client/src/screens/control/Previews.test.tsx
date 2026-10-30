// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { settle } from "../talk/send";
import type { SendAttempt } from "../../app/ui";
import type { AnnotationBundle, DeliveryState, PreviewEndpoint, Run } from "../../protocol/types";
import { PREVIEW_FOCUS_FRESH_MS, previewDeliveries, previewDeliveryFocus, recordPreviewSend } from "./previewDelivery";
import { RouteProvider } from "../../shell/Screen";
import { Shell } from "../../shell/Shell";
import { useStore } from "../../state/store";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import { nativePreview, pendingPreviews } from "../../host/preview";
import { allGrants } from "../../dev/sim/host";
import * as fx from "../../dev/sim/scenario";
import Previews, { PreviewCard, pictureTaskLabel, pictureTasks, pictureTarget, samePage } from "./Previews";
import { previewDrafts, type PreviewDraft } from "./previewDrafts";
import { previewAnnotation } from "./previewAnnotation";

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke, Channel: class {} }));
vi.mock("./previewAnnotation", async importOriginal => ({ ...await importOriginal<typeof import("./previewAnnotation")>(), previewAnnotation: vi.fn(async () => "annotation") }));

const preview = { preview_id: "preview", project_id: fx.ids.pTide, label: "Notes", url: "http://localhost:3000", reachable: true, checked_at: 1 };
type Descriptor = { id: string; size: number; taskId?: string | null; capturedAt?: number | null };
let app: App;
let delivered: Map<string, PreviewDraft>;
let pending: Map<string, Descriptor>;
let order: string[];
let dismiss: (result: unknown) => void;
/** Control's root, on screen while it is the current tab's top. */
function OnControl({ children }: { children: ReactNode }) {
  const active = useStore(app.ui, s => s.tab === "control" && s.stacks.control.length === 1);
  return <RouteProvider value={{ tab: "control", route: { screen: "control" }, depth: 0, active }}>{children}</RouteProvider>;
}
const mount = async (shown: PreviewEndpoint = preview) => {
  let mounted!: ReturnType<typeof render>;
  await act(async () => { mounted = render(<AppProvider app={app}><OnControl><PreviewCard preview={shown} /></OnControl></AppProvider>); });
  return mounted;
};

beforeEach(() => {
  localStorage.clear();
  previewDeliveries("pictures").set({});
  delivered = new Map(); pending = new Map(); order = [];
  vi.spyOn(previewDrafts, "list").mockImplementation(async () => [...delivered.values()]);
  vi.spyOn(previewDrafts, "put").mockImplementation(async draft => { order.push(`save:${draft.id}`); delivered.set(draft.id, draft); });
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const session = new HostSession("pictures", { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.link.set({ state: "online", since: Date.now() });
  session.state.set(s => ({ ...s, caughtUp: true, grants: allGrants, tasks: Object.fromEntries(fx.tasks(100).map(t => [t.task_id, t])) }));
  vi.spyOn(session, "command").mockResolvedValue({ command_id: "check", status: "accepted", seq: 1, error: null,
    result: { kind: "preview_registered", preview_id: preview.preview_id } });
  app = {
    session, ui: new UiStore("pictures"), profiles: [], switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {},
    profile: { profileId: "pictures", hostId: "host", name: "Computer", deviceId: "phone", addresses: [], certSha256: "", pairedAt: 0 },
    platform: { kind: "tauri", preview: nativePreview, pendingPreviews } as unknown as Platform,
  };
  app.ui.selectTask(fx.ids.pTide, fx.ids.tDash);
  native.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    switch (command) {
      case "preview_open": return "session";
      case "preview_show": return new Promise(resolve => { dismiss = resolve; });
      case "preview_close": order.push("close"); return;
      case "preview_pending": return { pictures: [...pending.values()] };
      case "preview_picture_read": return new Uint8Array([1, 2, 3]).buffer;
      case "preview_picture_ack": order.push(`ack:${args.id}`); pending.delete(args.id as string); return;
      default: throw Error(`Unexpected command ${command}`);
    }
  });
});
// Every test's store saves to the same key after a pause. Save it now, so a finished
// test cannot overwrite what a later test saved.
afterEach(() => { cleanup(); app.session.stop(); app.ui.persistNow(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals(); });

it("keeps the native session open until dismissal, then saves every picture and offers Annotate", async () => {
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  const pictures = [{ id: "first", size: 3, capturedAt: 100 }, { id: "second", size: 3, capturedAt: 200 }];
  for (const picture of pictures) pending.set(picture.id, picture);
  expect(delivered.size).toBe(0);
  expect(order).not.toContain("close");
  expect(native.invoke).toHaveBeenCalledWith("preview_show", expect.objectContaining({ taskId: fx.ids.tDash }));
  act(() => app.ui.selectTask(fx.ids.pTide, fx.ids.tFlaky));
  await act(async () => dismiss({ pictures }));
  expect(order).toEqual(["save:first", "ack:first", "save:second", "ack:second", "close"]);
  expect([...delivered.values()].map(d => [d.id, d.at, d.taskId])).toEqual([["first", 100, fx.ids.tDash], ["second", 200, fx.ids.tDash]]);
  expect(delivered.get("first")).toMatchObject({ profileId: "pictures", previewId: "preview", previewLabel: "Notes", computerId: "host", computerName: "Computer" });
  expect(pending.size).toBe(0);
  const annotate = screen.getAllByRole("button", { name: "Annotate picture" });
  expect(annotate).toHaveLength(2);
  await act(async () => fireEvent.click(annotate[0]!));
  expect(previewAnnotation).toHaveBeenCalledWith(delivered.get("first"), app.ui);
  expect(app.ui.route()).toEqual({ screen: "annotate", annotationId: "annotation" });
});

it("opens the editor with the picture chosen from the native preview's confirmation", async () => {
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  const pictures = [{ id: "first", size: 3, capturedAt: 100 }, { id: "second", size: 3, capturedAt: 200 }];
  for (const picture of pictures) pending.set(picture.id, picture);
  await act(async () => dismiss({ pictures, annotate: "second" }));
  expect(order).toEqual(["save:first", "ack:first", "save:second", "ack:second", "close", "save:second"]);
  expect(previewAnnotation).toHaveBeenCalledExactlyOnceWith(delivered.get("second"), app.ui);
  expect(app.ui.route()).toEqual({ screen: "annotate", annotationId: "annotation" });
});

it("annotates for the task running when Annotate is chosen, not when the preview opened", async () => {
  const sample = app.session.state.get().tasks[fx.ids.tDash]!;
  const working = { ...sample, state: "working" as const };
  app.session.state.set(s => ({ ...s, tasks: { [working.task_id]: working } }));
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  const next = { ...sample, task_id: "next", title: "next", state: "working" as const, updated_at: sample.updated_at + 1 };
  act(() => app.session.state.set(s => ({ ...s, tasks: { [working.task_id]: { ...working, state: "completed" }, next } })));
  pending.set("picture", { id: "picture", size: 3, capturedAt: 100 });
  await act(async () => dismiss({ pictures: [{ id: "picture", size: 3, capturedAt: 100 }], annotate: "picture" }));
  expect(previewAnnotation).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ id: "picture", taskId: "next" }), app.ui);
});

it.each([null, { id: "legacy", size: 3 }])("accepts older native close results (%j)", async result => {
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  await act(async () => dismiss(result));
  expect(delivered.size).toBe(result ? 1 : 0);
  expect(order.at(-1)).toBe("close");
});

it("leaves an undelivered capture recoverable when app storage fails", async () => {
  const mounted = await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  const pictures = [{ id: "saved", size: 3, taskId: fx.ids.tDash, capturedAt: 100 }, { id: "recover", size: 3, taskId: fx.ids.tDash, capturedAt: 200 }];
  for (const picture of pictures) pending.set(picture.id, picture);
  vi.mocked(previewDrafts.put).mockImplementationOnce(async d => { order.push(`save:${d.id}`); delivered.set(d.id, d); })
    .mockRejectedValueOnce(new Error("Storage full"));
  await act(async () => dismiss({ pictures }));
  expect(pending.has("saved")).toBe(false);
  expect(pending.has("recover")).toBe(true);
  expect(order).not.toContain("ack:recover");
  expect(screen.getByText("Storage full")).toBeTruthy();
  mounted.unmount();
  await mount();
  expect(pending.size).toBe(0);
  expect(delivered.get("recover")).toMatchObject({ at: 200, taskId: fx.ids.tDash, captureTimeKnown: true });
  expect(screen.getAllByRole("button", { name: "Annotate picture" })).toHaveLength(2);
  expect(order.indexOf("save:recover")).toBeLessThan(order.indexOf("ack:recover"));
});

it("recovers all captures after a crash, deduplicates saved IDs and reads old metadata", async () => {
  delivered.set("saved", { id: "saved", profileId: "pictures", previewId: "preview", projectId: fx.ids.pTide, taskId: fx.ids.tDash,
    blob: new Blob(["png"]), at: 99, artifactId: "attached" });
  pending.set("saved", { id: "saved", size: 3, capturedAt: 100 });
  pending.set("new", { id: "new", size: 3, taskId: fx.ids.tDash, capturedAt: 200 });
  pending.set("legacy", { id: "legacy", size: 3, taskId: null, capturedAt: null });
  await mount();
  expect(order).toEqual(["ack:saved", "save:new", "ack:new", "save:legacy", "ack:legacy"]);
  expect(delivered.get("saved")).toMatchObject({ artifactId: "attached", at: 99 });
  expect(delivered.get("new")).toMatchObject({ at: 200, taskId: fx.ids.tDash, captureTimeKnown: true });
  expect(delivered.get("legacy")).toMatchObject({ taskId: null, captureTimeKnown: false });
  expect(screen.getAllByRole("button", { name: "Annotate picture" })).toHaveLength(3);
  expect(pending.size).toBe(0);
});

it("says in plain words when nothing answered at the preview, and Check again opens it once it answers", async () => {
  const answered = native.invoke.getMockImplementation()!;
  let refused = true;
  native.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    if (command === "preview_open" && refused)
      throw { code: "conflict", message: "The computer hasn’t confirmed this preview is reachable. Check it again.", detail: { reason: null } };
    return answered(command, args);
  });
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  expect(screen.getByText("Computer couldn’t reach Notes")).toBeTruthy();
  expect(screen.getByText(/Start the preview’s server on Computer, then check again to open it/)).toBeTruthy();
  expect(screen.queryByText(/Conflict|private forward/)).toBeNull();
  expect(native.invoke).not.toHaveBeenCalledWith("preview_show", expect.anything());
  refused = false;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Check again" })));
  expect(vi.mocked(app.session.command).mock.calls.map(([body]) => body)).toEqual([
    { command: "check_preview", preview_id: "preview" }, { command: "check_preview", preview_id: "preview" }]);
  expect(native.invoke).toHaveBeenCalledWith("preview_show", expect.anything());
  expect(screen.queryByText("Computer couldn’t reach Notes")).toBeNull();
});

it("offers to start a shown preview's stopped server with the agent's command, then opens it once it answers", async () => {
  const answered = native.invoke.getMockImplementation()!;
  let refused = true;
  native.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    if (command === "preview_open" && refused)
      throw { code: "conflict", message: "The computer hasn’t confirmed this preview is reachable. Check it again.", detail: { reason: null } };
    return answered(command, args);
  });
  await mount({ ...preview, start: "npx vite --port 3000" });
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  expect(screen.getByText("Computer couldn’t reach Notes")).toBeTruthy();
  expect(screen.getByText("npx vite --port 3000")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
  refused = false;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Start server" })));
  expect(vi.mocked(app.session.command).mock.calls).toEqual([
    [{ command: "check_preview", preview_id: "preview" }, { label: "Check preview before opening" }],
    [{ command: "check_preview", preview_id: "preview", start: true }, { label: "Start the server for Notes" }]]);
  expect(native.invoke).toHaveBeenCalledWith("preview_show", expect.anything());
  expect(screen.queryByText("Computer couldn’t reach Notes")).toBeNull();
});

it("shows the start command without offering to run it to a device that can't open terminals", async () => {
  app.session.state.set(s => ({ ...s, grants: { ...allGrants, terminal: false } }));
  native.invoke.mockImplementation(async (command: string) => {
    if (command === "preview_open")
      throw { code: "conflict", message: "The computer hasn’t confirmed this preview is reachable. Check it again.", detail: { reason: null } };
    throw Error(`Unexpected command ${command}`);
  });
  await mount({ ...preview, start: "npx vite --port 3000" });
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  expect(screen.getByText("npx vite --port 3000")).toBeTruthy();
  expect(screen.getByText(/This device can’t open terminals there/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Start server" })).toBeNull();
  expect(screen.getByRole("button", { name: "Check again" })).toBeTruthy();
});

it("counts each saved web-preview picture and resets when reopened", async () => {
  let count = 0;
  app.platform.preview = async () => ({
    document: "<p>Preview</p>", close: async () => {}, show: async () => null,
    picture: async () => ({ id: `web-${++count}`, blob: new Blob(["picture"], { type: "image/png" }), acknowledge: async () => {} }),
  });
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Take picture" })));
  expect(screen.getByText("Picture saved. Take another or close the preview.")).toBeTruthy();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Take picture" })));
  expect(screen.getByText("2 pictures saved. Take another or close the preview.")).toBeTruthy();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Close preview" })));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  expect(screen.queryByText(/2 pictures saved/)).toBeNull();
});


it("shows where confirmed Continue sent a preview, keeps unknown sends unsent, and retains Annotate and Remove", async () => {
  const captureId = "sent-preview-u14";
  pending.set(captureId, { id: captureId, size: 3, taskId: fx.ids.tDash, capturedAt: 100 });
  previewDeliveries(app.profile.profileId).set({});
  const mounted = await mount();
  const bundle: AnnotationBundle = { bundle_id: "markup", revision: 1, sources: [{ key: "S1", artifact_id: "source", sha256: "0".repeat(64), media_type: "image/png", width: 600, height: 400, captured_at: 100, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: captureId, host_id: "host", computer_name: "Computer", preview_id: "preview", preview_label: "Notes", captured_at: 100 } }], marks: [], comments: [], steps: [], general_note: "Change the heading", speech: null };
  const attempt: SendAttempt = { phase: "unknown", at: 1000, commandId: "continue", text: "Change this", detail: "Waiting for computer", body: { command: "resume_task", task_id: fx.ids.tDash, text: "Change this", bundle } };
  expect(screen.getByRole("button", { name: "Attach to this task" })).toBeTruthy();
  act(() => settle(app, fx.ids.pTide, fx.ids.tDash, attempt, { command_id: "continue", status: "rejected", seq: null, result: null, error: null }));
  expect(screen.queryByText(/Sent with markup to/)).toBeNull();
  act(() => app.session.state.set(s => ({ ...s, timelines: { [fx.ids.tDash]: { loaded: true, hasMore: false, items: [{ seq: 10, at: 1000, run_id: null, item: { kind: "user_message", text: "Change this", bundle, delivery: "native_delivered" } }] } } })));
  act(() => settle(app, fx.ids.pTide, fx.ids.tDash, attempt, { command_id: "continue", status: "accepted", seq: 1, result: null, error: null }));
  const title = app.session.state.get().tasks[fx.ids.tDash]!.title;
  expect(screen.getByText(new RegExp(`Sent with markup to ${title}`))).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Attach to this task" })).toBeNull();
  expect(screen.queryByText("Saved on this device")).toBeNull();
  expect(screen.getByRole("button", { name: "Annotate picture" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Remove from this device…" })).toBeTruthy();
  expect(JSON.parse(localStorage.getItem("preview-deliveries.pictures")!)[captureId]).toMatchObject({ taskId: fx.ids.tDash, taskTitle: title, at: 1000 });
  mounted.unmount(); await mount();
  expect(screen.getByText(new RegExp(`Sent with markup to ${title}`))).toBeTruthy();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open task" })));
  expect(app.ui.get().taskByProject[fx.ids.pTide]).toBe(fx.ids.tDash);
});

it("lands focus on the picture's line once Start from its markup closes Review, and not later", async () => {
  const captureId = "started-preview-ur9";
  pending.set(captureId, { id: captureId, size: 3, taskId: null, capturedAt: 100 });
  const bundle: AnnotationBundle = { bundle_id: "markup", revision: 1, sources: [{ key: "S1", artifact_id: "source", sha256: "0".repeat(64), media_type: "image/png", width: 600, height: 400, captured_at: 100, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: captureId, host_id: app.profile.hostId, computer_name: "Computer", preview_id: "preview", preview_label: "Notes", captured_at: 100 } }], marks: [], comments: [], steps: [], general_note: "Change the heading", speech: null };
  const body = { command: "create_task" as const, project_id: fx.ids.pTide, kind: "coding" as const, title: null, prompt: "Change this", harness: "codex" as const, model: null, effort: null, mission: null, target_id: null, bundle, placement: { mode: "start_now" as const } };
  const attempt: SendAttempt = { phase: "unknown", at: 1000, commandId: "start", text: "Change this", detail: "Waiting for computer", body };
  const start = () => {
    app.ui.selectTab("control");
    app.ui.push({ screen: "review", projectId: fx.ids.pTide, taskId: null });
    app.ui.saveDraft(fx.ids.pTide, null, { text: "Change this", attempt });
  };
  const accepted = { command_id: "start", status: "accepted" as const, seq: 1, result: { kind: "task_created" as const, task_id: fx.ids.tDash, queue_item_id: null }, error: null };
  act(start);
  await mount();
  act(() => settle(app, fx.ids.pTide, null, attempt, accepted));
  expect(app.ui.get().stacks.control).toHaveLength(1);
  const line = screen.getByText(/Handed to .* not yet confirmed by the agent|Sent with markup to/);
  expect(line.getAttribute("role")).toBe("status");
  expect(document.activeElement).toBe(line);
  expect(previewDeliveryFocus(app.ui).get()).toBeNull();

  // A request the line couldn't take then must not move focus when it next shows.
  cleanup();
  act(() => previewDeliveryFocus(app.ui).set({ captureId, at: Date.now() - PREVIEW_FOCUS_FRESH_MS }));
  await mount();
  expect(document.activeElement).toBe(document.body);
  expect(previewDeliveryFocus(app.ui).get()).toBeNull();
});

it("keeps focus on the picture's line when the whole Control screen returns after Start, and takes it only when nothing there holds it", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const frames = () => act(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  const captureId = "started-preview-ur10";
  pending.set(captureId, { id: captureId, size: 3, taskId: null, capturedAt: 100 });
  const bundle: AnnotationBundle = { bundle_id: "markup", revision: 1, sources: [{ key: "S1", artifact_id: "source", sha256: "0".repeat(64), media_type: "image/png", width: 600, height: 400, captured_at: 100, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: captureId, host_id: app.profile.hostId, computer_name: "Computer", preview_id: "preview", preview_label: "Notes", captured_at: 100 } }], marks: [], comments: [], steps: [], general_note: "Change the heading", speech: null };
  const body = { command: "create_task" as const, project_id: fx.ids.pTide, kind: "coding" as const, title: null, prompt: "Change this", harness: "codex" as const, model: null, effort: null, mission: null, target_id: null, bundle, placement: { mode: "start_now" as const } };
  const attempt: SendAttempt = { phase: "unknown", at: 1000, commandId: "start", text: "Change this", detail: "Waiting for computer", body };
  app.session.state.set(s => ({ ...s, projects: Object.fromEntries(fx.projects(100).map(p => [p.project_id, p])), previews: { [fx.ids.pTide]: [preview] } }));
  act(() => app.ui.selectTab("control"));
  await act(async () => { render(<AppProvider app={app}><Shell /></AppProvider>); });
  // Its screens load lazily; wait for Control's previews, then Review on top.
  // Loading them takes about half a second alone and more beside the whole suite.
  fireEvent.click(await screen.findByRole("radio", { name: /^Previews/ }, { timeout: 4000 }));
  act(() => {
    app.ui.push({ screen: "review", projectId: fx.ids.pTide, taskId: null });
    app.ui.saveDraft(fx.ids.pTide, null, { text: "Change this", attempt });
  });
  await waitFor(() => {
    screen.getByRole("button", { name: "Annotate picture", hidden: true });
    screen.getByRole("heading", { level: 1, name: /^Review/, hidden: true });
  }, { timeout: 4000 });
  await frames();
  act(() => settle(app, fx.ids.pTide, null, attempt, { command_id: "start", status: "accepted", seq: 1, result: { kind: "task_created", task_id: fx.ids.tDash, queue_item_id: null }, error: null }));
  expect(app.ui.route()).toEqual({ screen: "control" });
  const line = screen.getByText(/Handed to .* not yet confirmed by the agent|Sent with markup to/);
  expect(document.activeElement).toBe(line);
  // Control's own arrival focus runs a frame later and must leave it there.
  await frames();
  expect(document.activeElement).toBe(line);

  // Arriving with nothing on the screen focused still lands on its title.
  act(() => { line.blur(); app.ui.selectTab("talk"); });
  act(() => app.ui.selectTab("control"));
  await frames();
  // Its title is the chosen project's name, the computer's under it (D-066).
  const project = app.session.state.get().projects[app.ui.get().projectId ?? ""]?.name ?? "Choose a project";
  expect(document.activeElement).toBe(screen.getByRole("heading", { level: 1, name: project }));
});

it("keeps the pending-message warning only while that message is pending", async () => {
  const captureId = "pending-preview-u29";
  pending.set(captureId, { id: captureId, size: 3, taskId: fx.ids.tDash, capturedAt: 100 });
  const bundle: AnnotationBundle = { bundle_id: "markup", revision: 1, sources: [{ key: "S1", artifact_id: "source", sha256: "0".repeat(64), media_type: "image/png", width: 600, height: 400, captured_at: 100, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: captureId, host_id: "host", computer_name: "Computer", preview_id: "preview", preview_label: "Notes", captured_at: 100 } }], marks: [], comments: [], steps: [], general_note: "Change the heading", speech: null };
  const attempt: SendAttempt = { phase: "unknown", at: 1000, commandId: "continue", text: "Change this", detail: "Waiting for computer", body: { command: "resume_task", task_id: fx.ids.tDash, text: "Change this", bundle } };
  app.ui.saveDraft(fx.ids.pTide, fx.ids.tDash, { text: "Change this", attempt });
  await mount();
  const actual = await vi.importActual<typeof import("./previewAnnotation")>("./previewAnnotation");
  vi.mocked(previewAnnotation).mockImplementationOnce(actual.previewAnnotation);
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Annotate picture" })));
  const warning = "Check the task’s pending message before changing its attached pictures.";
  expect(screen.getByText(warning)).toBeTruthy();
  // Refused by the computer, the message is still the task's to check.
  act(() => settle(app, fx.ids.pTide, fx.ids.tDash, attempt, { command_id: "continue", status: "rejected", seq: null, result: null, error: null }));
  expect(screen.getByText(warning)).toBeTruthy();
  act(() => app.session.state.set(s => ({ ...s, timelines: { [fx.ids.tDash]: { loaded: true, hasMore: false, items: [{ seq: 10, at: 1000, run_id: null, item: { kind: "user_message", text: "Change this", bundle, delivery: "native_delivered" } }] } } })));
  act(() => settle(app, fx.ids.pTide, fx.ids.tDash, attempt, { command_id: "continue", status: "accepted", seq: 1, result: null, error: null }));
  expect(screen.getByText(/Sent with markup to/)).toBeTruthy();
  expect(screen.queryByText(warning)).toBeNull();
});

it.each([
  ["queued_for_boundary", "Waiting to send to", "held until its turn ends", false],
  ["failed", "Not delivered to", "task stopped", true],
  ["cancelled", "Not delivered to", "cancelled", true],
  ["native_delivered", "Sent with markup to", "", false],
] as const)("follows message delivery: %s", async (state: DeliveryState, label, detail, retry) => {
  const taskId = fx.ids.tDash;
  pending.set("delivery-picture", { id: "delivery-picture", size: 3, taskId, capturedAt: 100 });
  await mount();
  const bundle: AnnotationBundle = { bundle_id: "delivery-markup", revision: 1, sources: [{ key: "S1", artifact_id: "source", sha256: "0".repeat(64), media_type: "image/png", width: 600, height: 400, captured_at: 100, derived_from: null, derivation: null,
    origin: { kind: "preview_capture", capture_id: "delivery-picture", host_id: "host", computer_name: "Computer", preview_id: "preview", preview_label: "Notes", captured_at: 100 } }], marks: [], comments: [], steps: [], general_note: null, speech: null };
  act(() => recordPreviewSend(app, { command: "send_message", task_id: taskId, text: "Picture", bundle, delivery: "next_turn" }, { command_id: "message", status: "accepted", seq: 1, result: null, error: null }));
  expect(screen.queryByText(/Sent with markup/)).toBeNull();
  act(() => app.session.state.set(s => ({ ...s,
    tasks: { ...s.tasks, [taskId]: { ...s.tasks[taskId]!, state: "interrupted" } },
    timelines: { [taskId]: { loaded: true, hasMore: false, items: [{ seq: 10, at: 1000, run_id: null, item: { kind: "user_message", text: "Picture", bundle, delivery: state } }] } },
  })));
  expect(screen.getByText(new RegExp(label)).textContent).toContain(detail);
  expect(!!screen.queryByRole("button", { name: "Put back in composer" })).toBe(retry);
  expect(JSON.parse(localStorage.getItem("preview-deliveries.pictures")!)["delivery-picture"].state).toBe(state);
  act(() => app.session.state.set(s => ({ ...s, timelines: {} })));
  expect(screen.getByText(new RegExp(label))).toBeTruthy();
  if (retry) {
    act(() => { app.ui.selectTask(fx.ids.pTide, taskId); app.ui.push({ screen: "receipt", taskId }, "talk"); app.ui.selectTab("control"); });
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
    expect(previewAnnotation).not.toHaveBeenCalled();
    expect(app.ui.draft(fx.ids.pTide, taskId)?.text).toBe("Picture");
    expect(app.ui.route()).toEqual({ screen: "talk" });
    expect(app.ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
  }
});

it("never treats a legacy acceptance record as delivered", async () => {
  pending.set("legacy-delivery", { id: "legacy-delivery", size: 3, taskId: fx.ids.tDash });
  previewDeliveries("pictures").set({ "legacy-delivery": { taskId: fx.ids.tDash, taskTitle: "wait", at: 100 } });
  app.session.state.set(s => ({ ...s, timelines: { [fx.ids.tDash]: { loaded: true, hasMore: false, items: [{ seq: 10, at: 1000, run_id: null, item: { kind: "user_message", text: "Unrelated", bundle: null, delivery: "native_delivered" } }] } } }));
  await mount();
  expect(screen.queryByText(/Sent with markup/)).toBeNull();
  expect(screen.getByText(/Delivery to .* is unconfirmed/)).toBeTruthy();
});

it("labels duplicate titles, orders running then recent tasks, and preselects a running task", async () => {
  const sample = app.session.state.get().tasks[fx.ids.tDash]!;
  const now = Date.now();
  const stopped = { ...sample, task_id: "stopped", title: "wait", state: "interrupted" as const, updated_at: now - 600_000 };
  const running = { ...sample, task_id: "running", title: "wait", state: "working" as const, updated_at: now - 1_200_000 };
  const recent = { ...sample, task_id: "recent", title: "wait", state: "completed" as const, updated_at: now };
  const tasks = [stopped, recent, running];
  expect(pictureTasks(tasks).map(t => t.task_id)).toEqual(["running", "recent", "stopped"]);
  expect(pictureTaskLabel(stopped)).toContain("wait · Stopped ·");
  expect(pictureTaskLabel(running)).toContain("wait · Working ·");
  const sampleRun = Object.values(app.session.state.get().runs)[0]!;
  const decided = { ...recent, task_id: "decided", current_run: "decided-run" };
  const decidedRun = { ...sampleRun, run_id: "decided-run", task_id: "decided", human_resolution: { resolution: "mark_completed" as const, at: now, by: { kind: "local_owner" as const } } };
  expect(pictureTaskLabel(decided, { "decided-run": decidedRun })).toContain("wait · Marked completed ·");
  expect(pictureTarget(tasks, "stopped")).toMatchObject({ taskId: "running", notice: expect.stringContaining("has ended") });
  expect(pictureTarget(tasks, "running")).toEqual({ taskId: "running", notice: null });
  app.session.state.set(s => ({ ...s, tasks: Object.fromEntries(tasks.map(t => [t.task_id, t])) }));
  pending.set("target-picture", { id: "target-picture", size: 3, taskId: "stopped", capturedAt: now });
  await mount();
  expect((screen.getByLabelText("Attach picture to") as HTMLSelectElement).value).toBe("running");
  expect(screen.getByText(/Selected .*has ended/)).toBeTruthy();
  expect(delivered.get("target-picture")?.taskId).toBe("running");
  app.ui.saveDraft(fx.ids.pTide, "running", { annotationId: "old-markup" });
  await act(async () => fireEvent.change(screen.getByLabelText("Attach picture to"), { target: { value: "recent" } }));
  expect(screen.getByText(/Any earlier markup is kept with/)).toBeTruthy();
  expect(app.ui.draft(fx.ids.pTide, "running")?.annotationId).toBe("old-markup");
  expect(delivered.get("target-picture")?.taskId).toBe("recent");
});
it("both retry entry points restore the immutable text and markup, and a draft choice preserves both", async () => {
  const { retryReceipt } = await import("../work/retryReceipt");
  const { annotationStore } = await import("../../state/annotations");
  const task = app.session.state.get().tasks[fx.ids.tDash]!;
  const bundle: AnnotationBundle = { bundle_id: "original", revision: 1, sources: [], marks: [], comments: [], steps: [], general_note: "Only the Today spacing", speech: null };
  const request = { item: { kind: "user_message" as const, text: "Do not edit typography or the Next card.", bundle, delivery: "failed" as const } };
  pending.set("retry", { id: "retry", size: 3, taskId: task.task_id });
  previewDeliveries("pictures").set({ retry: { taskId: task.task_id, taskTitle: task.title, at: 10, state: "failed", request: structuredClone(request.item) } });
  retryReceipt(app, task, request);
  const store = annotationStore("pictures");
  const receiptDraft = app.ui.draft(task.project_id, task.task_id)!;
  const receiptNote = store.get()[receiptDraft.annotationId!]!.generalNote;
  app.ui.clearDraft(task.project_id, task.task_id);
  bundle.general_note = "A later edit must not replace the submission";
  await mount();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  const previewDraft = app.ui.draft(task.project_id, task.task_id)!;
  expect(previewDraft.text).toBe(receiptDraft.text);
  expect(store.get()[previewDraft.annotationId!]!.generalNote).toBe(receiptNote);
  act(() => app.ui.saveDraft(task.project_id, task.task_id, { text: "Keep this different draft" }));
  const existing = app.ui.draft(task.project_id, task.task_id)!;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  expect(screen.getByRole("dialog", { name: "Choose a draft" })).toBeTruthy();
  expect(app.ui.draft(task.project_id, task.task_id)).toBe(existing);
  fireEvent.click(screen.getByRole("button", { name: "Keep current draft" }));
  expect(app.ui.draft(task.project_id, task.task_id)).toBe(existing);
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Use submitted request" })));
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe(request.item.text);
  const restored = new UiStore("pictures");
  expect(Object.values(restored.get().savedDrafts).flat()).toEqual([existing]);
  expect(store.get()[existing.annotationId!]).toBeTruthy();
  const { SavedRetryDrafts } = await import("../work/RetryRequest");
  render(<AppProvider app={app}><SavedRetryDrafts projectId={task.project_id} taskId={task.task_id} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Saved drafts · 1" }));
  fireEvent.click(screen.getByRole("button", { name: "Use this draft" }));
  expect(app.ui.draft(task.project_id, task.task_id)).toBe(existing);
  expect(Object.values(new UiStore("pictures").get().savedDrafts).flat()[0]?.text).toBe(request.item.text);
});

it("says once per project that a server may still serve the old version after a task changes files, until acknowledged", async () => {
  const [base] = fx.runs(100);
  const ended = (run_id: string, ended_at: number) => ({ ...base!, run_id, task_id: fx.ids.tDash, state: "completed" as const, ended_at,
    outcome: { summary: null, changed_files: [{ path: "tip.js", change: "modified", added: 1, removed: 1 }], checks: [], still_active: [], unknowns: [], usage: null } }) as Run;
  const title = app.session.state.get().tasks[fx.ids.tDash]!.title;
  act(() => app.session.state.set(s => ({ ...s, runs: { first: ended("first", Date.now() - 60_000) } })));
  const section = async () => {
    let mounted!: ReturnType<typeof render>;
    await act(async () => { mounted = render(<AppProvider app={app}><OnControl><Previews previews={[preview, { ...preview, preview_id: "other", label: "Storybook" }]} /></OnControl></AppProvider>); });
    return mounted;
  };
  const view = await section();
  expect(screen.getAllByText(`“${title}” changed files`, { exact: false })).toHaveLength(1);
  expect(screen.getByText(/its server may not reload changed files. Restart it on Computer/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Go to Terminals" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Got it" }));
  expect(screen.queryByText(/its server may not reload/)).toBeNull();
  view.unmount();
  await section();
  expect(screen.queryByText(/its server may not reload/)).toBeNull();
  act(() => app.session.state.set(s => ({ ...s, runs: { ...s.runs, second: ended("second", Date.now()) } })));
  expect(screen.getByText(/its server may not reload/)).toBeTruthy();
});

it("says a found preview's server is running or stopped, in its project", async () => {
  const projects = Object.fromEntries(fx.projects(100).map((p) => [p.project_id, p]));
  act(() => app.session.state.set((s) => ({ ...s, projects })));
  const found = { ...preview, automatic: true, url: "http://localhost:5173/", checked_at: Date.now() - 60_000 };
  const view = await act(async () => render(<AppProvider app={app}><OnControl><PreviewCard preview={found} /></OnControl></AppProvider>));
  const name = projects[fx.ids.pTide]!.name;
  expect(screen.getByText(`Running in ${name}`, { exact: false })).toBeTruthy();
  view.rerender(<AppProvider app={app}><OnControl><PreviewCard preview={{ ...found, reachable: false }} /></OnControl></AppProvider>);
  expect(screen.getByText(`Stopped running in ${name}`, { exact: false })).toBeTruthy();
  expect(screen.queryByText(/Could not be reached/)).toBeNull();
});

it("lists no found site that has stopped, though someone's own stays to be checked again", async () => {
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...fx.capabilities(), features: ["found_previews_v1"] } })));
  const found = { ...preview, automatic: true, url: "http://localhost:5173/", label: "Shop" };
  const section = (previews: PreviewEndpoint[]) => <AppProvider app={app}><OnControl><Previews previews={previews} /></OnControl></AppProvider>;
  const view = await act(async () => render(section([found, { ...found, preview_id: "stopped", label: "Old", reachable: false }])));
  expect(screen.getByRole("button", { name: "Open Shop" })).toBeTruthy();
  expect(screen.queryByText("Old")).toBeNull();
  view.rerender(section([{ ...found, reachable: false }]));
  expect(screen.getByText("No sites running")).toBeTruthy();
  view.rerender(section([{ ...preview, reachable: false }]));
  expect(screen.getByText(/Could not be reached/)).toBeTruthy();
});

it("says sites started in a project appear by themselves where the computer finds them", async () => {
  const section = () => render(<AppProvider app={app}><OnControl><Previews previews={[]} /></OnControl></AppProvider>);
  const view = await act(async () => section());
  expect(screen.getByText("No previews added")).toBeTruthy();
  view.unmount();
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...fx.capabilities(), features: ["found_previews_v1"] } })));
  await act(async () => section());
  expect(screen.getByText("No sites running")).toBeTruthy();
  expect(screen.getByText(/appears here by itself. Add one running elsewhere on Computer/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add a preview" })).toBeTruthy();
});

it("knows one page on the computer by any loopback name", () => {
  expect(samePage("http://localhost:5173/", "http://127.0.0.1:5173/")).toBe(true);
  expect(samePage("http://[::ffff:127.0.0.1]:80/a?b", "http://localhost/a?b")).toBe(true);
  // Each may be another server on the same port.
  expect(samePage("http://[::1]:5173/", "http://localhost:5173/")).toBe(false);
  expect(samePage("http://[::1]:5173/", "http://127.0.0.1:5173/")).toBe(false);
  expect(samePage("http://127.0.0.2:5173/", "http://127.0.0.1:5173/")).toBe(false);
  expect(samePage("http://[::ffff:127.0.0.2]:5173/", "http://localhost:5173/")).toBe(false);
  expect(samePage("http://[::1]:5173/", "http://[0:0::1]:5173/")).toBe(true);
  expect(samePage("http://localhost:5173/a", "http://localhost:5173/b")).toBe(false);
  expect(samePage("http://localhost:5173/", "http://localhost:5174/")).toBe(false);
  expect(samePage("http://localhost:5173/", "https://localhost:5173/")).toBe(false);
  expect(samePage("http://192.168.1.2/", "http://localhost/")).toBe(false);
  expect(samePage("http://192.168.1.2/", "http://192.168.1.2/")).toBe(true);
  expect(samePage("not a url", "not a url")).toBe(true);
});
