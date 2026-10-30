// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { commits, finderReview, prReviews, rulesIgnored } from "../../dev/sim/reviews";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Task } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import GithubList from "./GithubList";
import { PrSheet } from "./PrSheet";
import { ReviewResultCard, TaskFindings } from "./ReviewResult";

let app: App;
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
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function view(node: ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{node}<Toasts /></RouteProvider></AppProvider>);
}
const task = (id: string) => app.session.state.get().tasks[id]!;
const github = () => Object.values(app.session.commands.get()).flatMap((c) => (c.body.command === "github" ? [c.body.action] : []));
/** The tHeaders task with its pull request's review swapped for `github`'s. */
const withReview = (github: Partial<NonNullable<Task["github"]>>): Task => {
  const t = task(ids.tHeaders);
  return { ...t, github: { ...t.github!, ...github } };
};
const card = () => within(screen.getByRole("region", { name: /^Review result|^What the review found/ }));

it("leads a pull request with its review: corrected problems, the recheck, and what wasn't run", async () => {
  view(<PrSheet open onClose={() => {}} projectId={ids.pTide} number={214} task={task(ids.tHeaders)} />); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#214 Add rate-limit headers to /v1/orders" }));
  expect(sheet.getByText("Two confirmed problems were corrected. The corrections were reviewed and the pull request’s checks passed. One check was not run: the Android screen check.")).toBeTruthy();
  const facts = [...sheet.getByRole("region", { name: "Review result" }).querySelectorAll(".review-fact")].map((f) => f.textContent);
  expect(facts).toEqual([
    "ReviewDoneReviewed up to commit 5b7e2c9. That took two rounds of at most three.",
    "ChecksPassedAll three of its checks on GitHub passed.",
    "EvidenceMixedOne was shown by running it. One was found by reading the code, without running anything.",
    "Your approvalNot neededNothing it changes needs your approval. You still choose when to merge it.",
  ]);
  // No approval needed: the merge is the usual one.
  expect(sheet.getByRole("button", { name: "Squash and merge" })).toBeTruthy();
  expect(sheet.queryByRole("button", { name: "Approve and merge" })).toBeNull();
});

it("shows each finding with its verdict and what became of it, and the set-aside ones with why", async () => {
  view(<ReviewResultCard task={task(ids.tHeaders)} />);
  const c = card();
  fireEvent.click(c.getByText("What the review found and followed"));
  const part = (title: string | RegExp) => within(c.getByRole("heading", { name: title }).parentElement!);
  expect(part("Confirmed problems (2)").getAllByRole("button").map((b) => b.textContent)).toEqual([
    expect.stringMatching(/^Reset is sent as a timestamp, not seconds.*Confirmed — shown by running it.*src\/orders\/limits\.rs:58.*Corrected$/),
    expect.stringMatching(/^Remaining can go below zero.*Confirmed — from reading the code.*Corrected$/),
  ]);
  expect(part("Set aside").getByRole("button").textContent).toMatch(/^Headers are missing on 429 responses429 responses come from the shared limiter.*Not a problem$/);
  expect(part("Set aside").getByText(/A separate agent checked this one and couldn’t show it’s a real problem/)).toBeTruthy();
  expect(part("What it covered").getAllByRole("listitem").map((l) => l.textContent)).toEqual([
    "It reviewed the change up to commit 5b7e2c9.", "That is the pull request’s newest commit.",
    "Two rounds of review have been used, of at most three.", "This check was not run: the Android screen check.",
  ]);
  expect(part("Who reviewed it").getByText(/^Reviewed by Claude Code/)).toBeTruthy();
  expect(part("Who reviewed it").getByText(/^Each finding was checked by a separate agent: Codex/)).toBeTruthy();
  expect(part("Review rules").getByText("It followed the project’s review rules in these files: REVIEW.md and src/orders/REVIEW.md from commit 0a9b8c7 on origin/main.")).toBeTruthy();

  // A finding in full: its failure, the verdict and what it rests on.
  fireEvent.click(part("Confirmed problems (2)").getByRole("button", { name: /^Remaining can go below zero/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Remaining can go below zero under concurrent requests" }));
  expect(sheet.getByText("Confirmed — from reading the code")).toBeTruthy();
  expect(sheet.getByText(/It rests on reading the code: nothing was run to show it\./)).toBeTruthy();
  expect(sheet.getByText("Two requests arrive together when one call is left.")).toBeTruthy();
  expect(sheet.getByText("Found in round one at commit 1f2e3d4.")).toBeTruthy();
});

it("says a clean review found no problems, and that this doesn't prove the change right", async () => {
  view(<ReviewResultCard task={withReview({ review: prReviews.clean })} />);
  const c = card();
  expect(c.getByText("The review found no problems. The pull request’s checks passed.")).toBeTruthy();
  expect(c.getByText("No problem was confirmed, so there was nothing to show. That doesn’t prove the change is right.")).toBeTruthy();
  fireEvent.click(c.getByText("What the review found and followed"));
  expect(c.getByText(/It found nothing to report\. That doesn’t prove the change is right/)).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/proved correct|is correct|safe to merge/i);
});

it("says when the change's own edits to the review rules were ignored, and why no rules were used", async () => {
  view(<ReviewResultCard task={withReview({ review: { ...prReviews.clean, rules: rulesIgnored } })} />);
  expect(card().getByText("This change edits the review rules. Its edits were not followed; the rules already in the project were.")).toBeTruthy();
  cleanup();
  view(<ReviewResultCard task={withReview({ review: prReviews.setAside })} />);
  expect(card().getByText("No review rules were used. The project has no REVIEW.md.")).toBeTruthy();
  expect(card().getByText("The review found no confirmed problems. A second agent checked two possible problems and set them aside. The pull request’s checks passed.")).toBeTruthy();
  expect(card().getByText(/^It stops after five tries/)).toBeTruthy();
  expect(card().getAllByText("Couldn’t be settled").length).toBe(1);
});

it("says newer commits haven't been reviewed", async () => {
  const t = withReview({ review: prReviews.clean });
  view(<ReviewResultCard task={t} pr={{ ...t.github!.pr!, head: "c0ffee0000000000000000000000000000000000" }} />);
  expect(card().getByText("The review found no problems. The pull request’s checks passed. Newer commits haven’t been reviewed yet.")).toBeTruthy();
  fireEvent.click(card().getByText("What the review found and followed"));
  expect(card().getByText("The pull request has newer commits, up to c0ffee0. They haven’t been reviewed.")).toBeTruthy();
});

it("never says a review found nothing when no review has been recorded", async () => {
  view(<ReviewResultCard task={withReview({ review: { round: 0 }, approval: { areas: [{ area: "updates" }], head: commits.signing } })} />);
  const c = card();
  expect(c.getByText(/^No review of this change has been recorded\./)).toBeTruthy();
  fireEvent.click(c.getByText("What the review found and followed"));
  expect(c.queryByText(/found nothing/)).toBeNull();
  expect(c.getByText("No round of review has finished, so there is nothing to report.")).toBeTruthy();
  expect(document.body.textContent).not.toMatch(/hasn’t been reviewed yet/);
});

it("shows nothing for a pull request the computer recorded no review for", () => {
  view(<ReviewResultCard task={withReview({ review: undefined, approval: undefined })} />);
  expect(screen.queryByRole("region", { name: "Review result" })).toBeNull();
});

it("says why a stopped review needs the person", async () => {
  view(<PrSheet open onClose={() => {}} projectId={ids.pTide} number={220} task={task(ids.tExport)} />); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#220 Stream the orders export" }));
  expect(sheet.getByText("One confirmed problem is being fixed. One check on the pull request has failed. The review stopped and needs you to decide what happens next.")).toBeTruthy();
  expect(sheet.getByText("Why it needs you")).toBeTruthy();
  expect(sheet.getAllByText(/exports over 5,000 orders still stop at the first page/).length).toBeGreaterThan(0);
});

it("approves and merges exactly the commit shown, after naming what the change affects, behind the unlock", async () => {
  const confirm = vi.spyOn(app.platform.appLock!, "confirm");
  view(<PrSheet open onClose={() => {}} projectId={ids.pTide} number={218} task={task(ids.tSigning)} />); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#218 Sign update bundles before they're published" }));
  expect(sheet.getByText("The review found no problems. The pull request’s checks passed. This change affects updates, signing and the review rules themselves, so it needs your approval.")).toBeTruthy();
  expect(sheet.getByText("This change needs your approval. Approving merges commit 8c4d2e1 into main, and nothing newer. Your phone’s unlock confirms it.")).toBeTruthy();
  expect(sheet.queryByRole("button", { name: "Squash and merge" })).toBeNull();

  // Not now sends nothing.
  fireEvent.click(sheet.getByRole("button", { name: "Approve and merge" })); await tick();
  let approve = within(screen.getByRole("dialog", { name: "Approve commit 8c4d2e1?" }));
  expect(approve.getAllByRole("listitem").map((l) => l.textContent)).toEqual(["Updates", "Signing", "The review rules themselves"]);
  expect(approve.getByText("Approving merges commit 8c4d2e1 into main, and nothing newer. If another commit arrives first, it needs your approval again.")).toBeTruthy();
  fireEvent.click(approve.getByRole("button", { name: "Not now" })); await tick();
  expect(screen.queryByRole("dialog", { name: "Approve commit 8c4d2e1?" })).toBeNull();
  expect(github()).toEqual([]);

  fireEvent.click(sheet.getByRole("button", { name: "Approve and merge" })); await tick();
  approve = within(screen.getByRole("dialog", { name: "Approve commit 8c4d2e1?" }));
  fireEvent.click(approve.getByRole("button", { name: "Approve and merge" })); await tick();
  expect(confirm).toHaveBeenLastCalledWith("Approve and merge commit 8c4d2e1 of pull request #218");
  // It says what it was shown, so a reason the person never saw isn't approved.
  const shown = task(ids.tSigning).github!.approval!.areas;
  expect(shown.length).toBe(3);
  expect(github().at(-1)).toEqual({ action: "merge_pr", project_id: ids.pTide, repo: "samrivera/tidewater-api", number: 218, expected_head: commits.signing, method: "squash", approval_shown: shown });
  expect(screen.queryByRole("dialog", { name: "Approve commit 8c4d2e1?" })).toBeNull();
  expect(screen.getByText("Pull request #218 merged")).toBeTruthy();
});

it("won't approve a commit newer than the one the approval was worked out for", async () => {
  window.__sim!.host.github.prs.get(218)!.summary.head = "feedface00000000000000000000000000000000";
  view(<PrSheet open onClose={() => {}} projectId={ids.pTide} number={218} task={task(ids.tSigning)} />); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#218 / }));
  expect(sheet.getByText(/An older commit needed your approval\. Whether the newest one does is checked when you merge it\./)).toBeTruthy();
  expect(sheet.getByText(/An older commit needed your approval; if this one does too/)).toBeTruthy();
  // Not locked: it is a plain merge of the current commit; the computer asks again if it needs approval.
  expect(sheet.queryByRole("button", { name: "Approve and merge" })).toBeNull();
  expect(sheet.getByRole<HTMLButtonElement>("button", { name: "Squash and merge" }).disabled).toBe(false);
  // It was shown no approval of this commit, so it approves none.
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "merge_pr", number: 218, approval_shown: [] });
});

it("puts the agents' pull requests that need the person under Needs you, each with why", async () => {
  const route = { screen: "github_list", projectId: ids.pTide } as const;
  view(<GithubList route={route} />, route); await tick(); await tick();
  const section = (title: string) => within(screen.getByRole("heading", { name: title }).closest("section")!);
  const needs = section("Needs you").getAllByRole("button").map((b) => b.textContent);
  expect(needs).toEqual([
    expect.stringMatching(/^#218 Sign update bundles.*It affects updates, signing and the review rules themselves, so it needs your approval to merge\.Approve/),
    expect.stringMatching(/^#220 Stream the orders export.*Its review stopped and needs you to decide what happens next\.Your call/),
    expect.stringMatching(/^#217 Page the orders export/),
  ]);
  expect(section("From agents").getAllByRole("button").map((b) => b.textContent)).toEqual([expect.stringMatching(/^#214 /)]);

  // Merged on the computer's word, it no longer needs anyone.
  const host = window.__sim!.host;
  const t = host.tasks.find((x) => x.task_id === ids.tSigning)!;
  const merged = { ...t, github: { ...t.github!, pr: { ...t.github!.pr!, state: "merged" as const } }, revision: t.revision + 1 };
  host.tasks = host.tasks.map((x) => (x.task_id === t.task_id ? merged : x));
  host.emit({ type: "task_upserted", task: merged }); await tick();
  expect(section("Needs you").getAllByRole("button").map((b) => b.textContent?.slice(0, 4))).toEqual(["#220", "#217"]);
});

it("says what a finder's run filed and what it set aside, and why", async () => {
  view(<TaskFindings review={finderReview} />);
  const c = within(screen.getByRole("region", { name: "What the review found" }));
  expect(c.getByText("One confirmed problem, filed as an issue. One possible problem was set aside after a separate agent checked it.")).toBeTruthy();
  fireEvent.click(c.getByText("Each finding and why"));
  expect(c.getByRole("button", { name: /^Webhook retries skip the signature check/ }).textContent).toMatch(/Filed as issue #231$/);
  expect(c.getByRole("button", { name: /^Order totals round half down/ }).textContent).toMatch(/Banker's rounding is intended.*Not a problem$/);
  // The finder's own list doesn't add a disclaimer meant for a whole change.
  expect(c.queryByText(/doesn’t prove the change is right/)).toBeNull();
});

it("shows a task's pull request review on its result", async () => {
  const { OutcomeCard } = await import("../work/OutcomeCard");
  const t = task(ids.tHeaders);
  const run = app.session.state.get().runs[ids.rHeaders]!;
  const project = app.session.state.get().projects[ids.pTide]!;
  view(<OutcomeCard task={t} run={run} project={project} plan={null} writing={false}
    outcome={{ kind: "done", title: "Done", detail: null, items: [], doubts: [], newWork: [] } as never} />); await tick();
  expect(screen.getByRole("region", { name: "Review result" })).toBeTruthy();
  expect(screen.getByText(/^Two confirmed problems were corrected\./)).toBeTruthy();
  expect(screen.getByRole("button", { name: "See pull request #214" })).toBeTruthy();
});
