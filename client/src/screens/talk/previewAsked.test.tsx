// SPDX-License-Identifier: Apache-2.0
// What an agent asks about a preview it shows is answered beside the page,
// then, back in Talk, put in Next up for the task or left in Needs you.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { pickedFrom } from "../../host/preview";
import { HostSession } from "../../host/session";
import type { ApprovalQuestion, Shown, TaskState } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { previewDrafts } from "../control/previewDrafts";
import { PreviewCard } from "../control/Previews";
import { answerMessage, answerRoute, unsentAnswers } from "./previewAnswers";
import { ShownCard } from "./Shown";

const QUESTIONS: ApprovalQuestion[] = [
  { header: "Pay button", question: "Where should the Pay button sit?", multi_select: false, secret: false,
    options: [{ label: "Under the total", description: undefined }, { label: "Pinned to the bottom", description: "Always in reach" }] },
  { question: "Anything else feel off?", multi_select: false, secret: false, options: [] },
];
const AT = 1_800_000_000_000;
const PREVIEW = { preview_id: "pv", project_id: ids.pTide, label: "Checkout", url: "http://127.0.0.1:5173", reachable: true, checked_at: AT };
const SHOWN: Shown = { what: "preview", preview_id: "pv", label: "Checkout", url: PREVIEW.url, note: "Try paying.", questions: QUESTIONS };

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  window.__sim!.host.dispose();
  app.session.link.set({ state: "online", since: Date.now() });
  app.session.state.set(s => ({ ...s, previews: { ...s.previews, [ids.pTide]: [PREVIEW as never] } }));
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const show = (node: React.ReactNode) => render(
  <AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>{node}</RouteProvider></AppProvider>,
);
const setState = (state: TaskState) =>
  act(() => app.session.state.set(s => ({ ...s, tasks: { ...s.tasks, [ids.tDash]: { ...s.tasks[ids.tDash]!, state } } })));
// The computer takes the command, then records it.
const accepted = () => vi.spyOn(app.session, "command").mockImplementation(async () => {
  setTimeout(() => app.session.state.set(s => ({ ...s, commandStages: { ...s.commandStages, answers: { stage: "observed", result: null, error: null } as never } })), 50);
  return { command_id: "answers", status: "accepted", seq: 1, error: null, result: null };
});
const answer = () => {
  const sheet = screen.getByRole("dialog", { name: "Your answers" });
  fireEvent.click(within(sheet).getByRole("radio", { name: "Pinned to the bottom" }));
  fireEvent.change(within(sheet).getAllByRole("textbox")[1]!, { target: { value: "The total jumps when the code applies" } });
  return sheet;
};

it("writes the answers for the agent as each question it asked, then the answer", () => {
  expect(answerMessage("Checkout", QUESTIONS, [{ labels: [], own: "" }, { labels: [], own: " " }])).toBeNull();
  expect(answerMessage("Checkout", QUESTIONS, [{ labels: ["Pinned to the bottom"], own: "" }, { labels: [], own: "" }])).toBe(
    "My answers to your questions about the preview “Checkout”:\n\nWhere should the Pay button sit?\n→ Pinned to the bottom\n\nAnything else feel off?\n→ (no answer)",
  );
  expect(answerRoute(undefined)).toBeNull();
  const task = (state: TaskState) => ({ ...app.session.state.get().tasks[ids.tDash]!, state });
  for (const s of ["completed", "failed", "interrupted"] as const) expect(answerRoute(task(s))).toBe("next_up");
  for (const s of ["working", "dispatching", "awaiting_decision"] as const) expect(answerRoute(task(s))).toBe("next_turn");
  expect(answerRoute(task("archived"))).toBeNull();
});

it("keeps only what the questions offered from a native preview's answers", () => {
  expect(pickedFrom([{ labels: ["Pinned to the bottom", "Under the total", "Made up"], own: "x".repeat(5000) }, { labels: ["Any"], own: "Fine" }], QUESTIONS))
    .toEqual([{ labels: ["Pinned to the bottom"], own: "x".repeat(4000) }, { labels: [], own: "Fine" }]);
  expect(pickedFrom([{ labels: [], own: "" }], QUESTIONS)).toBeNull();
  expect(pickedFrom([{ labels: "Under the total", own: "" }, { labels: [], own: "" }], QUESTIONS)).toBeNull();
  expect(pickedFrom(null, QUESTIONS)).toBeNull();
});

it("leaves finished answers in Needs you, then puts them in Next up for a task that stopped", async () => {
  setState("completed");
  show(<ShownCard shown={SHOWN} taskId={ids.tDash} at={AT} />);
  expect(screen.getByText(/The agent asks 2 questions about this page/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Answer here" }));
  const sheet = answer();
  fireEvent.click(within(sheet).getByRole("button", { name: "Leave in Needs you" }));
  await tick();
  expect(screen.getByText("Your answers aren’t sent yet")).toBeTruthy();
  expect(screen.getByText("Pay button: Pinned to the bottom")).toBeTruthy();
  expect(unsentAnswers(ids.tDash)).toBe(true);

  // Kept on the device: the card shows them again after the app restarts.
  cleanup();
  show(<ShownCard shown={SHOWN} taskId={ids.tDash} at={AT} />);
  expect(screen.getByText("Your answers aren’t sent yet")).toBeTruthy();
  const command = accepted();
  const revision = app.session.state.get().tasks[ids.tDash]!.revision;
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put in Next up" })));
  await tick();
  expect(command).toHaveBeenCalledTimes(1);
  expect(command.mock.calls[0]![0]).toEqual({
    command: "queue_continuation", task_id: ids.tDash, authorization: "auto_start",
    text: "My answers to your questions about the preview “Checkout”:\n\nWhere should the Pay button sit?\n→ Pinned to the bottom\n\nAnything else feel off?\n→ The total jumps when the code applies",
  });
  expect(command.mock.calls[0]![1]).toMatchObject({ expectedRevision: revision });
  expect(screen.getByText(/You sent your answers/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Put in Next up" })).toBeNull();
  expect(unsentAnswers(ids.tDash)).toBe(false);
});

it("sends answers to a working task for after its turn", async () => {
  setState("working");
  show(<ShownCard shown={SHOWN} taskId={ids.tDash} at={AT} />);
  fireEvent.click(screen.getByRole("button", { name: "Answer here" }));
  const sheet = answer();
  expect(within(sheet).getByText("The agent is still working. It reads your answers after this turn.")).toBeTruthy();
  const command = accepted();
  await act(async () => fireEvent.click(within(sheet).getByRole("button", { name: "Send after this turn" })));
  await tick();
  expect(command.mock.calls[0]![0]).toMatchObject({ command: "send_message", task_id: ids.tDash, bundle: null, delivery: "next_turn" });
  expect(screen.getByText(/You sent your answers/)).toBeTruthy();
});

it("asks beside the page and submits the answers as the preview closes", async () => {
  vi.spyOn(previewDrafts, "list").mockResolvedValue([]);
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "check", status: "accepted", seq: 1, error: null, result: { kind: "preview_registered", preview_id: "pv" } });
  const close = vi.fn(async () => {});
  app.platform.preview = vi.fn(async () => ({ document: "<h1>Checkout</h1>", close, show: async () => null }));
  const onPicked = vi.fn(), onSubmit = vi.fn();
  const picked = [{ labels: ["Under the total"], own: "" }, { labels: [], own: "" }];
  const ask = { questions: QUESTIONS, picked: [{ labels: [], own: "" }, { labels: [], own: "" }], onPicked, onSubmit };
  const { rerender } = render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}><PreviewCard preview={PREVIEW as never} ask={ask} /></RouteProvider></AppProvider>);
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Checkout" })));
  const page = screen.getByRole("dialog", { name: "Checkout" });
  expect(within(page).getByText("The agent asks 2 questions")).toBeTruthy();
  expect((within(page).getByRole("button", { name: "Save answers" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(within(page).getByRole("radio", { name: "Under the total" }));
  expect(onPicked).toHaveBeenCalledWith(picked);
  rerender(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}><PreviewCard preview={PREVIEW as never} ask={{ ...ask, picked }} /></RouteProvider></AppProvider>);
  await act(async () => fireEvent.click(within(page).getByRole("button", { name: "Save answers" })));
  expect(close).toHaveBeenCalled();
  expect(onSubmit).toHaveBeenCalledWith(picked);
});

it("minimizes the questions to a bubble over the page and brings them back", async () => {
  vi.spyOn(previewDrafts, "list").mockResolvedValue([]);
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "check", status: "accepted", seq: 1, error: null, result: { kind: "preview_registered", preview_id: "pv" } });
  app.platform.preview = vi.fn(async () => ({ document: "<h1>Checkout</h1>", close: async () => {}, show: async () => null }));
  const ask = { questions: QUESTIONS, picked: [{ labels: ["Under the total"], own: "" }, { labels: [], own: "" }], onPicked: vi.fn(), onSubmit: vi.fn() };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}><PreviewCard preview={PREVIEW as never} ask={ask} /></RouteProvider></AppProvider>);
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Checkout" })));
  const page = screen.getByRole("dialog", { name: "Checkout" });
  fireEvent.click(within(page).getByRole("button", { name: "Minimize questions" }));
  const bubble = within(page).getByRole("button", { name: "Show the agent’s 2 questions, 1 not answered" });
  expect(document.activeElement).toBe(bubble);
  expect(within(page).queryByText("Where should the Pay button sit?")).toBeNull();
  expect(within(page).queryByRole("button", { name: "Save answers" })).toBeNull();
  expect(bubble.textContent).toBe("1");
  fireEvent.click(bubble);
  expect(within(page).getByText("Where should the Pay button sit?")).toBeTruthy();
  expect(document.activeElement).toBe(within(page).getByRole("button", { name: "Minimize questions" }));
  expect(within(page).getByRole("button", { name: "Save answers" })).toBeTruthy();
});
