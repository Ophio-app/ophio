// SPDX-License-Identifier: Apache-2.0
// Each stage of a send fails with one fixed sentence that says whether the
// request left. Only a lost answer says it may have arrived, and a request
// that never left stops warning once the computer is connected again.
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { CommandError, HostSession, NotConnectedError, OutcomeUnknownError } from "../../host/session";
import { TransferError } from "../../host/transfer";
import { RouteProvider } from "../../shell/Screen";
import { toBundle } from "../../state/annotations";
import Review from "./Review";
import { send, type SendRequest } from "./send";

vi.mock("../../state/annotations", async (actual) => ({ ...(await actual<typeof import("../../state/annotations")>()), toBundle: vi.fn() }));

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 2, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

const request = (annotationId: string | null = null, taskId = ids.tDash, draftTaskId: string | null = taskId): SendRequest => ({
  projectId: ids.pTide,
  draftTaskId,
  text: "Check the chart",
  annotationId,
  label: "Send",
  build: (bundle) => ({ command: "send_message", task_id: taskId, text: "Check the chart", bundle, delivery: "next_turn" }),
});

/** Sends and settles, returning what the draft kept. */
async function attempt(req: SendRequest) {
  const out = send(app, req, () => {});
  await advance(2500);
  const result = await out;
  expect(result.ok).toBe(false);
  if (result.ok) throw new Error("sent");
  expect(app.ui.draft(req.projectId, req.draftTaskId)?.attempt).toEqual(result.attempt);
  return result.attempt;
}

const requestsLeft = () => Object.keys(app.session.commands.get()).length;

const uploaded = (name: string) => `Couldn’t upload the markup. Nothing was sent to ${name}.`;
const markupCases: [string, () => Error, "not_delivered" | "failed", (name: string) => string][] = [
  ["not connected", () => new NotConnectedError(), "not_delivered", uploaded],
  ["dropped before the upload was answered", () => new OutcomeUnknownError("upload"), "not_delivered", uploaded],
  ["dropped mid-upload", () => new TransferError("Wi-Fi dropped. Resolve this, then retry to continue where it stopped.", "t1", true), "not_delivered", uploaded],
  ["refused", () => new CommandError({ code: "permission_denied", message: "This device can’t upload files." }, null), "failed", (n) => `${uploaded(n)} This device can’t upload files.`],
  ["failed on the computer", () => new TransferError("The disk is full.", "t1", false), "failed", (n) => `${uploaded(n)} The disk is full.`],
  ["not ready on this device", () => new Error("Add your typed comment before sending."), "failed", () => "Add your typed comment before sending."],
];

it.each(markupCases)("the markup stage says one thing when it is %s, and the request never leaves", async (_case, error, phase, detail) => {
  vi.mocked(toBundle).mockRejectedValueOnce(error());
  const before = requestsLeft();
  const a = await attempt(request("m1"));
  expect(a).toMatchObject({ phase, commandId: null, detail: detail(app.profile.name) });
  expect(a.detail).not.toMatch(/may have|answered|Resolve this/);
  expect(requestsLeft()).toBe(before);
});

it("a device that can’t record the send says so and sends nothing", async () => {
  vi.spyOn(app.platform.outbox!, "put").mockRejectedValueOnce(new Error("Storage full"));
  const before = requestsLeft();
  const a = await attempt(request());
  expect(a).toMatchObject({ phase: "failed", commandId: null, detail: `This device couldn’t record this send, so it wasn’t sent. Nothing reached ${app.profile.name}.` });
  expect(requestsLeft()).toBe(before);
});

it("a computer that isn’t connected is not sent, in one sentence", async () => {
  act(() => window.__sim!.offline());
  await advance();
  const a = await attempt(request());
  expect(a).toMatchObject({ phase: "not_delivered", commandId: null, detail: `${app.profile.name} isn’t connected, so nothing was sent.` });
});

it("a refusal says why and keeps the draft", async () => {
  const a = await attempt(request(null, "no-such-task"));
  expect(a).toMatchObject({ phase: "rejected", commandId: null, detail: "That task no longer exists" });
});

it("only a lost answer says the request may have arrived", async () => {
  window.__sim!.dropNextReceipt();
  const out = send(app, request(), () => {});
  await advance(300);
  const result = await out;
  if (result.ok) throw new Error("sent");
  expect(result.attempt).toMatchObject({ phase: "unknown", detail: `The connection dropped before ${app.profile.name} answered. It may have arrived.` });
  expect(result.attempt.commandId).not.toBeNull();
});

it("a request that never left stops warning once the computer is connected again", async () => {
  const name = app.profile.name;
  act(() => window.__sim!.offline());
  await advance();
  vi.mocked(toBundle).mockRejectedValueOnce(new NotConnectedError());
  app.ui.saveDraft(ids.pTide, null, { text: "Tidy the README" });
  render(<Review route={{ screen: "review", projectId: ids.pTide, taskId: null }} />, { wrapper });
  await attempt(request("m1", ids.tDash, null));
  const notice = () => document.querySelector(".composer__notice")!;
  expect(notice().classList).toContain("notice--warn");
  expect(notice().textContent).toBe(`Not sent${uploaded(name)}Send againReport`);
  expect(screen.queryByRole("alert")).toBeNull();

  act(() => window.__sim!.online());
  await advance(3000);
  expect(notice().classList).toContain("notice--info");
  expect(notice().querySelector(".notice__title")?.textContent).toBe("Not sent yet");
  expect(notice().querySelector(".notice__body")?.textContent).toBe(`${uploaded(name)} ${name} is connected now.`);
  // The draft and its way to send are kept; nothing else changed.
  expect(app.ui.draft(ids.pTide, null)?.attempt?.phase).toBe("not_delivered");
  expect(screen.getByRole("button", { name: "Send again" })).toBeTruthy();

  act(() => window.__sim!.offline());
  await advance();
  expect(notice().classList).toContain("notice--warn");
});
