// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { arranged, DONE_SHOWN_MS, luminance, nextItem, ticked } from "../../app/checklist";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { LiveRegions } from "../../design/Announcer";
import { HostSession } from "../../host/session";
import type { Checklist, ChecklistItem, ChecklistSummary } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import ChecklistScreen, { ChecklistBody, ChecklistEntry } from "./Checklist";
import Todo from "./Todo";
import { ChecklistRow, summaryWords } from "./RequestChecklist";

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
const sim = () => window.__sim!.host.checklists;
beforeEach(async () => {
  // Before "Turn on live payments" is due.
  localStorage.clear(); vi.useFakeTimers(); vi.setSystemTime(new Date("2026-10-18T09:00:00"));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  // The simulator hashes skill files; native digests don't settle under fake time.
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); });

function view(node: ReactNode, route: Route) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: route.screen === "todo" ? "todo" : "work", depth: 1, active: true, route }}>{node}<Toasts /></RouteProvider></AppProvider>);
}
const project = (id: string) => app.session.state.get().projects[id]!;
const openList = async () => {
  const route = { screen: "checklist", projectId: ids.pTide } as const;
  view(<ChecklistScreen route={route} />, route); await tick(); await tick();
};
/** An item's fold opener, by the title it starts with. */
const toggle = (title: string) => screen.getByRole("button", { name: (n) => n.startsWith(title) });
const item = (title: string) => within(toggle(title).closest("li")!);
const titles = () => [...document.querySelectorAll(".cl-item__title")].map((t) => t.textContent);

it("sits under New with what stops work and what waits on the person, and opens the checklist", async () => {
  view(<ChecklistRow project={project(ids.pTide)} />, { screen: "work" });
  await tick(); await tick();
  const row = screen.getByRole("button", { name: /Launch tidewater-api/ });
  expect(row.textContent).toContain("4 to do · next: Give the agent a Stripe test key · 1 blocker · an agent waits on one · a question asked");
  expect(row.classList.contains("checklist-row--blocker")).toBe(true);
  fireEvent.click(row);
  const ui = app.ui.get();
  expect(ui.stacks[ui.tab].at(-1)).toEqual({ screen: "todo" });
});

it("asks an agent for a checklist where there is none, from the built-in skill", async () => {
  view(<ChecklistRow project={project(ids.pHome)} />, { screen: "work" });
  await tick(); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Request checklist/ }));
  await tick(); await tick();
  const draft = app.ui.draft(ids.pHome, null)!;
  expect(draft.text).toMatch(/^Make a checklist of everything I have to do myself/);
  expect(draft.review?.start?.start).toMatchObject({ type: "skill", id: "checklist" });
});

it("opens the next item in place, most pressing first, and folds it away where it is when it's ticked off", async () => {
  await openList();
  expect(titles()).toEqual([
    "Give the agent a Stripe test key", "Put the webhook secret on the server", "Choose the currency for prices",
    "Turn on live payments", "Test the Stripe webhook after access is ready", "Point api.tidewater.dev at the server",
  ]);
  expect(toggle("Give the agent a Stripe test key").getAttribute("aria-expanded")).toBe("true");
  expect(toggle("Put the webhook secret on the server").getAttribute("aria-expanded")).toBe("false");
  expect(toggle("Give the agent a Stripe test key").textContent).toMatch(/Blocker.*An agent waits on it/);
  expect(toggle("Put the webhook secret on the server").textContent).toContain("High priority");
  expect(toggle("Point api.tidewater.dev at the server").textContent).not.toContain("Low priority");
  expect(toggle("Turn on live payments").textContent).toMatch(/High priority.*Aim for Nov\s2/);

  const stripe = item("Give the agent a Stripe test key");
  // The page each step needs opens from the step itself.
  const opened = vi.spyOn(app.platform, "openExternal").mockResolvedValue(undefined);
  fireEvent.click(stripe.getByRole("button", { name: "Open dashboard.stripe.com" }));
  expect(opened).toHaveBeenCalledWith("https://dashboard.stripe.com/test/apikeys");
  fireEvent.click(stripe.getByRole("checkbox", { name: "Step 2" })); await tick();
  expect(sim().resumed).toEqual([]);
  fireEvent.click(stripe.getByRole("checkbox", { name: "Step 3" })); await tick(); await tick();
  expect(sim().resumed).toEqual([{ itemId: "stripe-key", waiting: "Sets up the Stripe webhook and runs a test payment" }]);
  // Nothing else opens; Next offers the one after.
  expect(toggle("Give the agent a Stripe test key").getAttribute("aria-expanded")).toBe("false");
  expect(toggle("Put the webhook secret on the server").getAttribute("aria-expanded")).toBe("false");
  expect(stripe.getByRole("checkbox", { name: "Open “Give the agent a Stripe test key” again" }).getAttribute("aria-checked")).toBe("true");
  expect(toggle("Give the agent a Stripe test key").textContent).not.toContain("Blocker");
  fireEvent.click(screen.getByRole("button", { name: /^Next: Put the webhook secret on the server/ })); await tick();
  expect(toggle("Put the webhook secret on the server").getAttribute("aria-expanded")).toBe("true");
  expect(item("Put the webhook secret on the server").getByText(/Get the file below/)).toBeTruthy();
});

it("opens a fold without closing another, and brings the next item up where the one ticked off was", async () => {
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  expect(toggle("Give the agent a Stripe test key").getAttribute("aria-expanded")).toBe("true");
  expect(toggle("Put the webhook secret on the server").getAttribute("aria-expanded")).toBe("true");

  // Scrolled down to Done, past the item's heading: once ticked off it goes
  // down to Done, and the item after it comes up to the top of the screen.
  const head = toggle("Give the agent a Stripe test key").closest<HTMLElement>(".cl-item__head")!;
  const after = toggle("Put the webhook secret on the server").closest<HTMLElement>(".cl-item__head")!;
  const scroller = head.closest<HTMLElement>(".screen__body")!;
  let scrolled = 500;
  Object.defineProperty(scroller, "scrollTop", { configurable: true, get: () => scrolled, set: (v: number) => { scrolled = v; } });
  const at = (top: number) => ({ top, bottom: top, left: 0, right: 0, width: 0, height: 0, x: 0, y: top, toJSON() {} }) as DOMRect;
  vi.spyOn(scroller, "getBoundingClientRect").mockImplementation(() => at(60));
  vi.spyOn(head, "getBoundingClientRect").mockImplementation(() => at(400 - scrolled));
  vi.spyOn(after, "getBoundingClientRect").mockImplementation(() => at(700 - scrolled));
  const scrolledTo = vi.fn();
  head.scrollIntoView = scrolledTo;
  fireEvent.click(item("Give the agent a Stripe test key").getByRole("button", { name: "Done" })); await tick();
  expect(toggle("Give the agent a Stripe test key").getAttribute("aria-expanded")).toBe("false");
  expect(toggle("Put the webhook secret on the server").getAttribute("aria-expanded")).toBe("true");
  expect(700 - scrolled).toBe(60);
  expect(scrolledTo).not.toHaveBeenCalled();
  expect(titles().slice(-3)).toEqual(["Test the Stripe webhook after access is ready", "Give the agent a Stripe test key", "Point api.tidewater.dev at the server"]);

  // A fold opened where it can be seen stays exactly there.
  scrolled = 100;
  fireEvent.click(toggle("Give the agent a Stripe test key")); await tick();
  expect(scrolled).toBe(100);
});

it("finishes an item that asks once it's answered", async () => {
  await openList();
  fireEvent.click(toggle("Choose the currency for prices")); await tick();
  const currency = item("Choose the currency for prices");
  expect((currency.getByRole("button", { name: "Answer above to finish" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(currency.getByRole("radio", { name: /^Euros/ }));
  fireEvent.click(currency.getByRole("button", { name: "Send answer" })); await tick(); await tick();
  const saved = sim().lists.get(ids.pTide)!.sections[1]!.items.find((i) => i.item_id === "currency")!;
  expect(saved.ask?.answer?.text).toBe("Euros");
  expect(saved.done_at).toBeDefined();
  // Done, it folds away and the one open above it stays open.
  expect(currency.getByRole("checkbox", { name: "Open “Choose the currency for prices” again" }).getAttribute("aria-checked")).toBe("true");
  expect(toggle("Choose the currency for prices").getAttribute("aria-expanded")).toBe("false");
  expect(toggle("Give the agent a Stripe test key").getAttribute("aria-expanded")).toBe("true");
  fireEvent.click(toggle("Choose the currency for prices")); await tick();
  expect(currency.getByText(/You answered: Euros/)).toBeTruthy();
});

it("takes a question under an item and shows the agent's answer there", async () => {
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  const secret = item("Put the webhook secret on the server");
  expect(secret.getByText(/Do “Give the agent a Stripe test key” first/)).toBeTruthy();
  fireEvent.click(secret.getByRole("button", { name: "Ask about this" }));
  fireEvent.change(secret.getByLabelText("Ask about this"), { target: { value: "Which server is it?" } });
  fireEvent.click(secret.getByRole("button", { name: "Ask" })); await tick(); await tick();
  expect(secret.getByText("Which server is it?")).toBeTruthy();
  expect(secret.getByText(/The agent answers here/)).toBeTruthy();
  sim().reply(ids.pTide, "webhook-secret", "The one api.tidewater.dev points at, 203.0.113.24."); await tick(); await tick();
  expect(secret.getByText(/The one api\.tidewater\.dev points at/)).toBeTruthy();
  expect(secret.queryByText(/The agent answers here/)).toBeNull();
});

it("marks agent work without tick or answer controls, and sends feedback to that exact item", async () => {
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  view(<ChecklistScreen route={route} />, route); await tick(); await tick();
  const person = item("Review the proposed topic tags");
  expect(toggle("Review the proposed topic tags").textContent).toContain("For you");
  expect(toggle("Review the proposed topic tags").textContent).toContain("An agent waits on it");
  const agent = item("Apply the approved tags");
  expect(toggle("Apply the approved tags").textContent).toContain("For agent");
  expect(agent.getByRole("button", { name: "Waiting on you: Review the proposed topic tags" })).toBeTruthy();
  expect(screen.getByRole("progressbar", { name: "Done" }).getAttribute("aria-valuemax")).toBe("1");
  expect(screen.getByText("1 for agent")).toBeTruthy();
  expect(agent.queryByRole("checkbox")).toBeNull();
  fireEvent.click(toggle("Apply the approved tags")); await tick();
  expect(agent.queryByRole("checkbox")).toBeNull();
  expect(agent.queryByRole("button", { name: "Done" })).toBeNull();
  expect(agent.queryByRole("button", { name: "Open it again" })).toBeNull();
  expect(agent.getByText(/Waiting on you to finish/)).toBeTruthy();
  fireEvent.click(agent.getByRole("button", { name: "Waiting on you: Review the proposed topic tags" })); await tick();
  expect(toggle("Review the proposed topic tags").getAttribute("aria-expanded")).toBe("true");
  fireEvent.click(agent.getByRole("button", { name: "Send feedback" }));
  fireEvent.change(agent.getByLabelText("Send feedback"), { target: { value: "Use the shorter tags" } });
  fireEvent.click(agent.getByRole("button", { name: "Send feedback" })); await tick(); await tick();
  const saved = sim().lists.get(ids.pNotes)!.sections[0]!.items.find((i) => i.item_id === "apply-tags")!;
  expect(saved.notes?.at(-1)?.text).toBe("Use the shorter tags");
  expect(screen.getByText("Feedback sent. The answer comes here.")).toBeTruthy();
  expect(saved.done_at).toBeUndefined();
  fireEvent.click(person.getByRole("button", { name: "Done" })); await tick(); await tick();
  expect(agent.queryByRole("button", { name: /Waiting on you/ })).toBeNull();
});

it("keeps All's project link separate from its fold button and names the agent awaiting an item", async () => {
  const original = sim().lists.get(ids.pNotes)!;
  const list: Checklist = { ...original, sections: original.sections.map((section) => ({ ...section,
    items: section.items.map((i) => i.item_id === "review-tags" ? { ...i, task_id: undefined } : i),
  })) };
  const person = list.sections[0]!.items[0]!;
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  const label = <button type="button">Field notes project</button>;
  view(<ol><ChecklistEntry list={list} project={project(ids.pNotes)} item={person}
    send={async () => null} projectLabel={label} /></ol>, route);
  const projectButton = screen.getByRole("button", { name: "Field notes project" });
  expect(projectButton.closest(".cl-item__toggle")).toBeNull();
  const fold = toggle("Review the proposed topic tags");
  expect(fold.contains(projectButton)).toBe(false);
  expect(fold.textContent).toContain("An agent waits on it");
  fireEvent.click(fold);
  expect(screen.getByText(/Once it’s done, the agent on “.+” carries on\./)).toBeTruthy();
});

it("names open agent to-dos before removal and stays on To-Do afterward", async () => {
  app.ui.openChecklist(ids.pNotes);
  const route = { screen: "todo" } as const;
  view(<Todo route={route} />, route); await tick(); await tick();
  fireEvent.click(screen.getByRole("button", { name: /Remove the checklist/ })); await tick();
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.getByText("2 items are still to do.", { exact: false })).toBeTruthy();
  expect(dialog.getByText("Apply the approved tags")).toBeTruthy();
  fireEvent.click(dialog.getByRole("button", { name: "Remove" })); await tick(); await tick();
  expect(sim().lists.has(ids.pNotes)).toBe(false);
  expect(app.ui.get().tab).toBe("todo");
  expect(screen.getByText("No checklist yet")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Request checklist" })).toBeTruthy();
});

it("tells the person to wait for an agent prerequisite instead of asking them to do it", () => {
  const first = entry({ item_id: "agent-first", title: "Prepare report", owner: "agent" });
  const later = entry({ item_id: "person-after", title: "Review report", after: [first.item_id] });
  const list: Checklist = { project_id: ids.pNotes, title: "Report", created_at: at, updated_at: at, revision: 1,
    sections: [{ section_id: "s", title: "Tasks", items: [first, later] }] };
  const go = vi.fn();
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  view(<ol><ChecklistEntry list={list} project={project(ids.pNotes)} item={later} send={async () => null} onGo={go} /></ol>, route);
  const fold = item("Review report");
  expect(toggle("Review report").textContent).toContain("Waiting for the agent");
  fireEvent.click(toggle("Review report"));
  expect(fold.getByText("The agent must finish “Prepare report” first.")).toBeTruthy();
  fireEvent.click(fold.getByRole("button", { name: "Go to it" }));
  expect(go).toHaveBeenCalledWith("agent-first");
});

it("names the dependent agent's writer over the person item's earlier writer", () => {
  const original = sim().lists.get(ids.pNotes)!;
  const list = { ...original, sections: original.sections.map((section) => ({ ...section,
    items: section.items.map((i) => i.item_id === "review-tags" ? { ...i, task_id: ids.tHeaders }
      : i.item_id === "apply-tags" ? { ...i, task_id: ids.tNotes } : i),
  })) };
  expect(list.sections[0]!.items[1]).toMatchObject({ owner: "agent", task_id: ids.tNotes, after: ["review-tags"] });
  const previous = app.session.state.get().tasks[ids.tNotes]!;
  act(() => app.session.state.set((state) => ({ ...state, tasks: {
    ...state.tasks,
    [ids.tNotes]: { ...previous, task_id: ids.tNotes, title: "Actual waiting agent" },
    [ids.tHeaders]: { ...previous, task_id: ids.tHeaders, title: "Earlier writer" },
  } })));
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  view(<ol><ChecklistEntry list={list} project={project(ids.pNotes)} item={list.sections[0]!.items[0]!} send={async () => null} /></ol>, route);
  const waiting = "Actual waiting agent";
  const earlier = "Earlier writer";
  const fold = toggle("Review the proposed topic tags");
  expect(fold.textContent).toContain("An agent waits on it");
  expect(fold.textContent).not.toContain(waiting);
  fireEvent.click(fold);
  const body = item("Review the proposed topic tags").getByText(/carries on/).textContent!;
  expect(body).toContain(`“${waiting}”`);
  expect(body).not.toContain(earlier);
});

it("acknowledges a completed person item from All and tells its waiting agent", () => {
  const list = sim().lists.get(ids.pNotes)!;
  const person = list.sections[0]!.items[0]!;
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  view(<ol><ChecklistEntry list={list} project={project(ids.pNotes)} item={person} send={async () => null} focus /></ol>, route);
  fireEvent.click(item("Review the proposed topic tags").getByRole("button", { name: "Done" }));
  expect(toggle("Review the proposed topic tags").getAttribute("aria-expanded")).toBe("false");
  expect(screen.getByText("Done. The agent waiting on it carries on now.")).toBeTruthy();
});

it("does not say everything is done while person items still wait on agent prerequisites", () => {
  const readyItem = entry({ item_id: "ready", title: "Send the brief" });
  const agentItem = entry({ item_id: "agent", title: "Write the draft", owner: "agent" });
  const blockedItem = entry({ item_id: "blocked", title: "Review the draft", after: [agentItem.item_id] });
  const list: Checklist = { project_id: ids.pNotes, title: "Draft", created_at: at, updated_at: at, revision: 1,
    sections: [{ section_id: "s", title: "Work", items: [readyItem, agentItem, blockedItem] }] };
  const route = { screen: "checklist", projectId: ids.pNotes } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
    <ol><ChecklistEntry list={list} project={project(ids.pNotes)} item={readyItem} send={async () => null} focus /></ol>
    <LiveRegions />
  </RouteProvider></AppProvider>);
  fireEvent.click(item("Send the brief").getByRole("button", { name: "Done" }));
  expect(screen.getByText("Done. The remaining items are waiting on other to-dos.")).toBeTruthy();
  expect(screen.queryByText(/Everything for you is done/)).toBeNull();
});

it("gets a step's file the way the person chose", async () => {
  sim().delivery = { delivery: { kind: "folder", path: "/home/sam/Google Drive" }, revision: 1 };
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  const secret = item("Put the webhook secret on the server");
  fireEvent.click(secret.getByRole("button", { name: "Put in Google Drive" })); await tick(); await tick();
  expect(secret.getByText("Sent to /home/sam/Google Drive/tidewater-api/tidewater.env")).toBeTruthy();
});

/** Downloads check a digest the simulator also makes; a stand-in that,
 * unlike the native one, settles under fake time. */
const digests = () => vi.stubGlobal("crypto", {
  getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto),
  subtle: { digest: async (_: string, data: ArrayBuffer | Uint8Array) => {
    const d = new Uint8Array(32);
    (ArrayBuffer.isView(data) ? data : new Uint8Array(data)).forEach((b, i) => { d[i % 32] = (d[i % 32]! * 31 + b) & 255; });
    return d.buffer;
  } },
});

it("opens a step's picture inside Ophio and saves it from there with one download", async () => {
  digests(); vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:picture");
  const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  await sim().ready;
  const list = sim().lists.get(ids.pTide)!;
  const step = list.sections.flatMap((s) => s.items).find((i) => i.item_id === "webhook-secret")!.steps[0]!;
  step.file = list.theme!.logo!;
  const downloads = vi.spyOn(app.session, "command");
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  fireEvent.click(item("Put the webhook secret on the server").getByRole("button", { name: "Open" })); await tick(); await tick();
  const viewer = within(screen.getByRole("dialog", { name: "tidewater.svg" }));
  expect(viewer.getByRole("img", { name: "tidewater.svg" }).getAttribute("src")).toBe("blob:picture");
  fireEvent.click(viewer.getByRole("button", { name: "Show at full size" }));
  expect(viewer.getByRole("button", { name: "Fit to the screen" })).toBeTruthy();
  fireEvent.click(viewer.getByRole("button", { name: "Save to this phone" })); await tick();
  expect(viewer.getByText("Saved to Downloads/tidewater.svg")).toBeTruthy();
  expect(downloads.mock.calls.filter(([c, o]) => c.command === "create_download" && o?.label === "Get tidewater.svg")).toHaveLength(1);
  fireEvent.click(viewer.getByRole("button", { name: "Close" })); await tick();
  expect(screen.queryByRole("dialog", { name: "tidewater.svg" })).toBeNull();
  expect(revoke).toHaveBeenCalledWith("blob:picture");
});

it("opens a step's text file to read", async () => {
  digests(); vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:text");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  fireEvent.click(item("Put the webhook secret on the server").getByRole("button", { name: "Open" })); await tick(); await tick();
  const viewer = within(screen.getByRole("dialog", { name: "tidewater.env" }));
  expect(viewer.getByText(/STRIPE_WEBHOOK_SECRET=/)).toBeTruthy();
});

it("offers only Save for a file Ophio can't show", async () => {
  await sim().ready;
  const step = sim().lists.get(ids.pTide)!.sections.flatMap((s) => s.items).find((i) => i.item_id === "webhook-secret")!.steps[0]!;
  step.file = { ...step.file!, name: "contract.pdf", media_type: "application/pdf" };
  await openList();
  fireEvent.click(toggle("Put the webhook secret on the server")); await tick();
  const secret = item("Put the webhook secret on the server");
  expect(secret.getByRole("button", { name: "Save" })).toBeTruthy();
  expect(secret.queryByRole("button", { name: "Open" })).toBeNull();
});

const at = 1_000;
const step = (step_id: string, done = false) => ({ step_id, text: step_id, ...(done ? { done_at: at } : {}) });
const entry = (over: Partial<ChecklistItem> & Pick<ChecklistItem, "item_id">): ChecklistItem =>
  ({ title: over.item_id, priority: "medium", steps: [], added_at: at, changed_at: at, ...over });

it("ticks as the computer does, and finds the next item it can do", () => {
  const asks = entry({ item_id: "a", steps: [step("1", true), step("2")], ask: { question: "Which?" } });
  const all = ticked(asks, "2", true, 5);
  expect(all.steps.every((s) => s.done_at !== undefined) && all.done_at === undefined).toBe(true);
  const answered = { ...asks, ask: { question: "Which?", answer: { text: "This", at } } };
  expect(ticked(answered, "2", true, 5).done_at).toBe(5);
  expect(ticked({ ...answered, done_at: at }, "1", false, 5).done_at).toBeUndefined();

  const list = (items: ChecklistItem[]): Checklist =>
    ({ project_id: ids.pTide, title: "T", sections: [{ section_id: "s", title: "S", items }], created_at: at, updated_at: at, revision: 1 });
  const later = entry({ item_id: "later", after: ["first"] });
  expect(nextItem(list([later, entry({ item_id: "first" })]))?.item_id).toBe("first");
  expect(nextItem(list([later, entry({ item_id: "first", done_at: at })]))?.item_id).toBe("later");
});

it("lifts an item whose day has come to just under the blockers, with what it needs first", () => {
  const sections = (...ss: [string, ChecklistItem[]][]): Checklist =>
    ({ project_id: ids.pTide, title: "T", sections: ss.map(([id, items]) => ({ section_id: id, title: id, items })), created_at: at, updated_at: at, revision: 1 });
  const order = (l: Checklist) => l.sections.flatMap((s) => s.items.map((i) => i.item_id));
  const noon = (day: string) => new Date(`${day}T12:00:00`).getTime();
  const list = sections(
    ["setup", [entry({ item_id: "block", priority: "blocker" }), entry({ item_id: "high", priority: "high" }), entry({ item_id: "prep", priority: "low" })]],
    ["later", [entry({ item_id: "soon", priority: "high" }), entry({ item_id: "launch", priority: "low", due: "2026-11-02", after: ["prep"] })]],
  );
  expect(order(arranged(list, "2026-11-01"))).toEqual(["block", "high", "soon", "prep", "launch"]);
  expect(order(arranged(list, "2026-11-02"))).toEqual(["block", "prep", "launch", "high", "soon"]);
  // Its section comes up above one with no blocker in it.
  const noBlocker = sections(["setup", [entry({ item_id: "high", priority: "high" })]], ["later", list.sections[1]!.items]);
  expect(order(arranged(noBlocker, "2026-11-03"))).toEqual(["launch", "high", "soon"]);
  // Ticked off on its day or before, it goes down to Done all the same.
  const done = (when: string) => sections(["s", [entry({ item_id: "a", priority: "high" }), entry({ item_id: "b", due: "2026-11-02", done_at: noon(when) })]]);
  expect(order(arranged(done("2026-11-02"), "2026-11-05", { now: noon("2026-11-05") }))).toEqual(["a", "b"]);
  expect(order(arranged(done("2026-10-30"), "2026-11-01", { now: noon("2026-11-01") }))).toEqual(["a", "b"]);
});

const now = new Date("2026-10-18T09:00:00").getTime();
const hours = (n: number) => now - n * 3_600_000;
const signing = (): Checklist => ({ project_id: ids.pTide, title: "Launch", created_at: at, updated_at: at, revision: 1, sections: [
  { section_id: "signing", title: "Release signing", items: [entry({ item_id: "key", title: "Make the release key", done_at: hours(2) })] },
  { section_id: "accounts", title: "Accounts", items: [
    entry({ item_id: "apple", title: "Join the developer programme" }), entry({ item_id: "play", title: "Pay the Play fee", done_at: hours(1) }),
    entry({ item_id: "old", title: "Choose the name", done_at: now - DONE_SHOWN_MS - 60_000 }),
    entry({ item_id: "recent", title: "Pick the icon", done_at: now - DONE_SHOWN_MS + 60_000 }),
  ] },
] });

it("puts done items in one Done group at the end, the latest first, and drops a section left with nothing open", () => {
  const shown = arranged(signing(), "2026-10-18", { now });
  expect(shown.sections.map((s) => [s.title, s.items.map((i) => i.item_id)])).toEqual([
    ["Accounts", ["apple"]], ["Done", ["play", "key", "recent"]],
  ]);
  // Opened again, it goes back to its own section.
  const reopened = { ...signing(), sections: signing().sections.map((s) => ({ ...s, items: s.items.map((i) => i.item_id === "key" ? ticked(i, undefined, false, now) : i) })) };
  expect(arranged(reopened, "2026-10-18", { now }).sections.map((s) => [s.title, s.items.map((i) => i.item_id)])).toEqual([
    ["Release signing", ["key"]], ["Accounts", ["apple"]], ["Done", ["play", "recent"]],
  ]);
  // Over three days done, it shows only when opened by link.
  expect(arranged(signing(), "2026-10-18", { now, keep: new Set(["old"]) }).sections.at(-1)!.items.map((i) => i.item_id)).toEqual(["play", "key", "recent", "old"]);
});

it("hides items done over three days ago but counts them in the bar, and shows one opened by link", () => {
  const route = { screen: "checklist", projectId: ids.pTide } as const;
  const body = (focus?: string) => <AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
    <ChecklistBody list={signing()} project={project(ids.pTide)} send={async () => null} focus={focus} />
  </RouteProvider></AppProvider>;
  const rendered = render(body());
  expect(titles()).toEqual(["Join the developer programme", "Pay the Play fee", "Make the release key", "Pick the icon"]);
  expect([...document.querySelectorAll(".cl-section__title")].map((t) => t.textContent)).toEqual(["Accounts", "Done", "Questions"]);
  const bar = screen.getByRole("progressbar", { name: "Done" });
  expect([bar.getAttribute("aria-valuenow"), bar.getAttribute("aria-valuemax")]).toEqual(["4", "5"]);
  expect(screen.getByText("1 for you to do")).toBeTruthy();
  rendered.unmount();
  render(body("old"));
  expect(titles().at(-1)).toBe("Choose the name");
  expect(toggle("Choose the name").getAttribute("aria-expanded")).toBe("true");
});

it("takes a ticked item down to Done at once, and back to its section when opened again", async () => {
  await openList();
  const heads = () => [...document.querySelectorAll(".cl-sectioned-items .cl-section__title")].map((t) => t.textContent);
  expect(heads()).toEqual(["Accounts and access", "Choices", "Accounts and access", "Done"]);
  vi.spyOn(app.session, "command").mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(item("Give the agent a Stripe test key").getByRole("button", { name: "Done" }));
  expect(titles()).toEqual([
    "Put the webhook secret on the server", "Choose the currency for prices", "Turn on live payments",
    "Test the Stripe webhook after access is ready", "Give the agent a Stripe test key", "Point api.tidewater.dev at the server",
  ]);
  await tick(); await tick();
  fireEvent.click(item("Give the agent a Stripe test key").getByRole("checkbox", { name: "Open “Give the agent a Stripe test key” again" })); await tick(); await tick();
  expect(titles()[0]).toBe("Give the agent a Stripe test key");
  expect(titles().at(-1)).toBe("Point api.tidewater.dev at the server");
});

it("globally puts cross-section prerequisites before blockers, earlier due work first, and Next on the prerequisite", () => {
  const p = entry({ item_id: "prep", priority: "low" });
  const b = entry({ item_id: "block", priority: "blocker", after: ["prep"] });
  const h = entry({ item_id: "high", priority: "high" });
  const later = entry({ item_id: "later-due", due: "2026-10-18" });
  const earlier = entry({ item_id: "earlier-due", due: "2026-10-17" });
  const list: Checklist = { project_id: ids.pTide, title: "Cross section", created_at: at, updated_at: at, revision: 1,
    sections: [
      { section_id: "first", title: "First", items: [b, h] },
      { section_id: "second", title: "Second", items: [p, later, earlier] },
    ] };
  const sorted = arranged(list, "2026-10-18");
  expect(sorted.sections.flatMap((section) => section.items.map((i) => i.item_id))).toEqual([
    "prep", "block", "earlier-due", "later-due", "high",
  ]);
  expect(sorted.sections.map((section) => section.title)).toEqual(["Second", "First", "Second", "First"]);
  expect(nextItem(list)?.item_id).toBe("prep");
});

it("preserves an open feedback draft when global ordering splits its section", async () => {
  const original = sim().lists.get(ids.pTide)!;
  const route = { screen: "checklist", projectId: ids.pTide } as const;
  const body = (list: Checklist) => <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
      <ChecklistBody list={list} project={project(ids.pTide)} send={async () => null} /><Toasts />
    </RouteProvider>
  </AppProvider>;
  const rendered = render(body(original));
  fireEvent.click(toggle("Put the webhook secret on the server"));
  const secret = item("Put the webhook secret on the server");
  fireEvent.click(secret.getByRole("button", { name: "Ask about this" }));
  const draft = secret.getByLabelText("Ask about this") as HTMLTextAreaElement;
  fireEvent.change(draft, { target: { value: "Please check the endpoint first" } });
  const reprioritized = { ...original, sections: original.sections.map((section) => ({ ...section,
    items: section.items.map((i) => i.item_id === "currency" ? { ...i, priority: "blocker" as const } : i),
  })) };
  rendered.rerender(body(reprioritized));
  expect(item("Put the webhook secret on the server").getByLabelText("Ask about this")).toBe(draft);
  expect(draft.value).toBe("Please check the endpoint first");
});

it("counts blockers on the row", () => {
  const s: ChecklistSummary = { project_id: ids.pTide, title: "T", open: 3, done: 1, waiting: 0, blockers: 2, unanswered: 0, updated_at: at, revision: 1 };
  expect(summaryWords(s)).toBe("3 to do · 2 blockers");
  expect(summaryWords({ ...s, open: 0, blockers: 0 })).toBe("All 1 done");
  expect(luminance("#ffffff")).toBeCloseTo(1);
  expect(luminance("#000000")).toBe(0);
  expect(luminance("teal")).toBeNull();
});
