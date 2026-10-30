// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { fixAllPrompt, fixOnePrompt, reviewPrompt } from "../../app/findings";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Task, TaskState } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Work from "../work/Work";
import Findings from "./Findings";
import NewFindings from "./NewFindings";

let app: App;
const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };
const opus = { harness: "claude_code" as const, model: "claude-opus-5-5", effort: null };
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  localStorage.setItem(`github.roles.${profile.profileId}`, JSON.stringify({ [ids.pGarden]: { find: opus, fix: astra } }));
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function view(node: ReactNode, route: Route) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>{node}<Toasts /></RouteProvider></AppProvider>);
}
const creates = () => Object.values(app.session.commands.get()).flatMap((c) => (c.body.command === "create_task" ? [c.body] : []));
const hub = async () => {
  const route = { screen: "findings", projectId: ids.pGarden } as const;
  view(<Findings route={route} />, route); await tick(); await tick();
};
const newOne = async (fix: boolean) => {
  const route = { screen: "findings_new", projectId: ids.pGarden, fix } as const;
  view(<NewFindings route={route} />, route); await tick();
};
const withoutFeature = () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "findings_v1") } }));
};

it("opens Find & fix on a project without GitHub, from its doors in Tasks", async () => {
  const route = { screen: "work" } as const;
  app.ui.selectProject(ids.pGarden);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
  await tick();
  const doors = within(screen.getByRole("heading", { name: "Project" }).closest("section")!).getAllByRole("button").map((b) => b.textContent);
  expect(doors.filter((d) => /^(Issues|Pull requests)/.test(d ?? ""))).toEqual([]);
  fireEvent.click(screen.getByRole("button", { name: /^Find & fix/ }));
  const ui = app.ui.get();
  expect(ui.stacks[ui.tab].at(-1)).toEqual({ screen: "findings", projectId: ids.pGarden });
});

it("lists FINDINGS.md, open first, and fixes one finding at a time", async () => {
  await hub();
  expect(screen.getByRole("heading", { name: "Find & fix", level: 1 })).toBeTruthy();
  const section = (title: string) => within(screen.getByRole("heading", { name: title }).closest("section")!);
  expect(section("Open").getAllByRole("button").map((b) => b.textContent)).toEqual([
    expect.stringMatching(/^2\. Deleting a bed leaves its plantings behind.*Confirmed — from reading the code.*Medium/),
  ]);
  expect(section("Set aside").getAllByRole("button").map((b) => b.textContent)).toEqual([
    expect.stringMatching(/^3\. The seed list.*Each button gets an aria-label.*Not a problem/),
  ]);
  expect(section("Done").getByRole("button").textContent).toContain("Fixed: frost.ts reads");
  expect(screen.getByRole("button", { name: /^Fix all/ }).textContent).toContain("the open finding, in order, and leaves the set-aside ones");
  fireEvent.click(section("Open").getByRole("button", { name: /^2\./ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "2. Deleting a bed leaves its plantings behind" }));
  expect(sheet.getByText(/removeBed drops the bed/)).toBeTruthy();
  expect(sheet.queryByText(/^Severity:/)).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Fix this one" })); await tick();
  const fix = within(screen.getByRole("dialog", { name: "Fix 2. Deleting a bed leaves its plantings behind" }));
  expect(fix.getByRole("button", { name: /^Fixed by/ }).textContent).toContain("GPT-6-Astra");
  fireEvent.change(fix.getByLabelText("More direction"), { target: { value: "Keep the migration reversible." } });
  fireEvent.click(fix.getByRole("button", { name: "Start" })); await tick();
  expect(creates()).toEqual([expect.objectContaining({
    project_id: ids.pGarden, mode: "work", harness: "codex", model: "gpt-6-astra", placement: { mode: "start_now" },
    title: "Fix finding 2: Deleting a bed leaves its plantings behind",
  })]);
  const prompt = creates()[0]!.prompt;
  expect(prompt.startsWith(fixOnePrompt({ number: 2, title: "Deleting a bed leaves its plantings behind" }))).toBe(true);
  expect(prompt).toContain("Keep the migration reversible.");
  expect(creates()[0]).not.toHaveProperty("github");
});

it("shows a finding's code example as written, with no row or fix for the headings in it, and none hidden after it", async () => {
  const { findings } = await import("../../dev/sim/scenario");
  const saved = findings[ids.pGarden]!;
  const sample = ["# Example document", "## [ ] 99. This is sample input", "Severity: high", "Where: sample.ts", ""].join("\n");
  try {
    findings[ids.pGarden] = saved
      .replace("added a test for Hobart.", "added a test for Hobart:\n\n- ```sh\n  npm test -- frost\n  ```\n")
      .replace("removeBed drops the bed", `Given this file:\n\n\`\`\`md\n${sample}\`\`\`\n\nremoveBed drops the bed`);
    await hub();
    const open = within(screen.getByRole("heading", { name: "Open" }).closest("section")!);
    expect(open.getAllByRole("button").map((b) => b.textContent)).toEqual([expect.stringMatching(/^2\. .*Medium/)]);
    expect(within(screen.getByRole("heading", { name: "Set aside" }).closest("section")!).getAllByRole("button").map((b) => b.textContent))
      .toEqual([expect.stringMatching(/^3\. /)]);
    expect(screen.queryByRole("button", { name: /^99\./ })).toBeNull();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: /^Fix all/ }).disabled).toBe(false);
    expect(screen.getByRole("button", { name: /^Fix all/ }).textContent).toContain("the open finding");
    expect(within(screen.getByRole("heading", { name: "Done" }).closest("section")!).getAllByRole("button")).toHaveLength(1);
    fireEvent.click(open.getByRole("button", { name: /^2\./ })); await tick();
    const sheet = screen.getByRole("dialog", { name: "2. Deleting a bed leaves its plantings behind" });
    expect(sheet.querySelector("pre code")!.textContent).toBe(sample);
    expect(within(sheet).getByText(/removeBed drops the bed/)).toBeTruthy();
    expect(within(sheet).getByText("src/beds/store.ts:88", { exact: false })).toBeTruthy();
  } finally { findings[ids.pGarden] = saved; }
});

it("puts Fix all in Next up for later", async () => {
  await hub();
  fireEvent.click(screen.getByRole("button", { name: /^Fix all/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Fix all" })).getByRole("button", { name: "Add to Next up" })); await tick();
  expect(creates()).toEqual([expect.objectContaining({ prompt: fixAllPrompt(), placement: { mode: "queue", position: null, authorization: "auto_start" } })]);
});

it("says when there's no FINDINGS.md yet, and when every box is ticked", async () => {
  const { findings } = await import("../../dev/sim/scenario");
  const saved = findings[ids.pGarden]!;
  try {
    delete findings[ids.pGarden];
    await hub();
    expect(screen.getByText("No FINDINGS.md yet")).toBeTruthy();
    expect(screen.getByRole<HTMLButtonElement>("button", { name: /^Fix all/ }).disabled).toBe(true);
    cleanup();
    findings[ids.pGarden] = saved.replaceAll("## [ ]", "## [x]");
    await hub();
    expect(screen.getByText("Every finding is ticked")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Open" })).toBeNull();
  } finally { findings[ids.pGarden] = saved; }
});

it("reviews into FINDINGS.md, then queues the fix to start once the review is done", async () => {
  app.ui.push({ screen: "findings", projectId: ids.pGarden }, "work");
  app.ui.push({ screen: "findings_new", projectId: ids.pGarden, fix: true });
  await newOne(true);
  expect(screen.getByRole("heading", { name: "Review and fix", level: 1 })).toBeTruthy();
  // No version control here, so no recent changes to review.
  expect(screen.queryByRole("radio", { name: "Recent changes" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Security researcher" }));
  fireEvent.click(screen.getByRole("radio", { name: "A folder" }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start review" }).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Folder"), { target: { value: "../elsewhere" } });
  expect(screen.getByText("A folder inside the project")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Folder"), { target: { value: "src/beds" } });
  fireEvent.click(screen.getByRole("button", { name: "One fewer" }));
  expect(screen.queryByRole("button", { name: "Edit first" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick(); await tick();
  const [review, fix] = creates();
  expect(review).toMatchObject({
    mode: "work", harness: "claude_code", placement: { mode: "start_now" }, title: "Find security issues in src/beds for FINDINGS.md",
    prompt: reviewPrompt({ focus: "security", where: { scope: "folder", path: "src/beds" }, most: 9, checked: true }),
    // The computer checks findings, so the review gets the project's review rules.
    review: "find",
  });
  // It fixes only what the second agent confirms.
  expect(fix).toMatchObject({ mode: "work", harness: "codex", prompt: fixAllPrompt({ checked: true }), placement: { mode: "queue", position: null, authorization: "auto_start" } });
  // The fixer is an ordinary task: only the review gets the review rules.
  expect(fix).not.toHaveProperty("review");
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "findings", projectId: ids.pGarden });
});

it("lets the fix wait to be started, and Review only starts the review alone", async () => {
  await newOne(true);
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "After the review" })).getByRole("radio", { name: "Ask first" }));
  expect(screen.getByText(/waits in Next up until you start it/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick(); await tick();
  expect(creates()[1]).toMatchObject({ placement: { mode: "queue", position: null, authorization: "ask_before_start" } });
  cleanup();
  await newOne(false);
  expect(screen.getByRole("heading", { name: "Review only", level: 1 })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Fixes/ })).toBeNull();
  expect(screen.getByRole("button", { name: "Edit first" })).toBeTruthy();
  // The first review holds the project, so this one waits its turn.
  expect(screen.getByText(/The review starts then\./)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start review when free" })); await tick(); await tick();
  expect(creates()).toHaveLength(3);
  expect(creates()[2]).toMatchObject({
    prompt: reviewPrompt({ focus: null, where: { scope: "project" }, most: 10, checked: true }), placement: { mode: "queue", position: null, authorization: "auto_start" },
  });
});

it("asks for an update where the computer can't keep findings in a file", async () => {
  withoutFeature();
  await hub();
  expect(screen.getByText(/can’t keep findings in a file yet/)).toBeTruthy();
  cleanup();
  await newOne(true);
  expect(screen.queryByRole("button", { name: "Start review" })).toBeNull();
});

/** A review without GitHub in the garden, and the second agent checking it, in `state`. */
function checking(state: TaskState) {
  const host = window.__sim!.host;
  const base = host.tasks.find((t) => t.task_id === ids.tNotes)!;
  const review: Task = {
    ...base, task_id: "0192f7a1-0000-7000-8000-00000000f101", project_id: ids.pGarden, title: "Find issues for FINDINGS.md",
    state: "completed", review: { role: "find", verified_by: "0192f7a1-0000-7000-8000-00000000f102" },
  };
  const checker: Task = {
    ...base, task_id: "0192f7a1-0000-7000-8000-00000000f102", project_id: ids.pGarden, title: "Check the findings in FINDINGS.md",
    state, created_at: review.created_at + 1, review: { role: "verify", of_task: review.task_id },
  };
  host.tasks = [...host.tasks, review, checker];
  for (const task of [review, checker]) host.emit({ type: "task_upserted", task });
}

it("says a second agent is checking the findings, and holds Fix all until it's done unless the person starts anyway", async () => {
  checking("working");
  await hub();
  expect(screen.getByText("A second agent is checking the findings")).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Fix all/ }).textContent).toContain("Waits until the second agent has checked them");
  fireEvent.click(screen.getByRole("button", { name: /^Fix all/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Fix all" }));
  expect(sheet.getByText("The findings are still being checked")).toBeTruthy();
  expect(sheet.getByText(/It leaves the one set aside alone\./)).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Start anyway" })); await tick();
  expect(creates()).toEqual([expect.objectContaining({ prompt: fixAllPrompt(), placement: { mode: "start_now" } })]);
});

it("says when the check stopped before it finished, and doesn't hold Fix all for it", async () => {
  checking("failed");
  await hub();
  expect(screen.queryByText("A second agent is checking the findings")).toBeNull();
  expect(screen.getByText("The check stopped before it finished")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Fix all/ })); await tick();
  expect(within(screen.getByRole("dialog", { name: "Fix all" })).getByRole("button", { name: "Start" })).toBeTruthy();
});

it("shows why a finding was set aside, and still fixes it when the person chooses it", async () => {
  await hub();
  fireEvent.click(screen.getByRole("button", { name: /^3\. The seed list/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "3. The seed list has no label for screen readers" }));
  expect(sheet.getByText("Set aside: not a problem")).toBeTruthy();
  expect(sheet.getByText(/each button gets an aria-label with the seed’s name|each button gets an aria-label with the seed's name/)).toBeTruthy();
  // The Verdict line is shown above the text, not repeated in it.
  expect(sheet.queryByText(/^Verdict:/)).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Fix it anyway" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Fix 3. The seed list has no label for screen readers" })).getByRole("button", { name: "Start" })); await tick();
  expect(creates()[0]!.prompt.startsWith(fixOnePrompt({ number: 3, title: "The seed list has no label for screen readers" }))).toBe(true);
});

it("says a finding hasn't been checked where the computer checks findings, and nothing where it doesn't", async () => {
  const { findings } = await import("../../dev/sim/scenario");
  const saved = findings[ids.pGarden]!;
  try {
    findings[ids.pGarden] = saved.replace(/^Verdict:.*$/gm, "");
    await hub();
    const open = within(screen.getByRole("heading", { name: "Open" }).closest("section")!);
    expect(open.getAllByRole("button").map((b) => b.textContent)).toEqual([
      expect.stringMatching(/^2\. .*Not checked yet/), expect.stringMatching(/^3\. .*Not checked yet/),
    ]);
    cleanup();
    const caps = app.session.state.get().capabilities!;
    app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "review_rules_v1") } }));
    await hub();
    expect(screen.queryByText(/Not checked yet/)).toBeNull();
  } finally { findings[ids.pGarden] = saved; }
});

it("leaves the review rules out of a review for a computer that doesn't check findings", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "review_rules_v1") } }));
  await newOne(false);
  expect(screen.queryByText(/A second agent then checks each one/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick(); await tick();
  expect(creates()[0]).not.toHaveProperty("review");
  expect(creates()[0]!.prompt).toBe(reviewPrompt({ focus: null, where: { scope: "project" }, most: 10 }));
  expect(creates()[0]!.prompt).not.toContain("Verdict");
});
