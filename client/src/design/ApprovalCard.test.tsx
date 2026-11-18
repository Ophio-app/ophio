// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { approvalCases as cases, approvals } from "../dev/sim/scenario";
import { ApprovalCard, ApprovalDetailText } from "./ApprovalCard";
import { plainToolNames } from "./toolNames";
import { clockTime } from "./time";
afterEach(cleanup);
it("disables both offline decisions with the same connection explanation", () => {
  const onDecide = vi.fn();
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId="phone" canDecide
    disabled disabledReason="Connect to the computer to answer this request." onDecide={onDecide} />);
  for (const name of ["Reject", "Allow"]) {
    const button = screen.getByRole("button", { name });
    expect(button).toHaveProperty("disabled", true);
    fireEvent.click(button);
  }
  expect(screen.getByText("Connect to the computer to answer this request.")).toBeTruthy();
  expect(screen.queryByLabelText("Reason (optional)")).toBeNull();
  expect(screen.queryByRole("button", { name: "Reject with a reason" })).toBeNull();
  expect(onDecide).not.toHaveBeenCalled();
});

it("drops the unconfirmed-decision warning once reconnecting lets the request be answered again", async () => {
  const unconfirmed = "The computer hasn’t confirmed your decision. Reconnect to reconcile it.";
  const onDecide = vi.fn().mockRejectedValueOnce({ error: { code: "outcome_unknown", message: unconfirmed } });
  const approval = approvals(Date.now())[0]!;
  const card = (disabled: boolean) => <ApprovalCard approval={approval} agent="Claude Code" myDeviceId="phone" canDecide
    disabled={disabled} disabledReason={unconfirmed} onDecide={onDecide} />;
  const { rerender } = render(card(false));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Allow" })); });
  rerender(card(true));
  // Said once, as the alert, while the outcome is still being learned.
  expect(screen.getAllByText(unconfirmed)).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe(unconfirmed);
  rerender(card(false));
  expect(screen.queryByText(unconfirmed)).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", false);
});

it("rejects in one tap, showing it on its way, with no reason asked for", async () => {
  let settle!: () => void;
  const onDecide = vi.fn(() => new Promise<void>(done => { settle = done; }));
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  const reject = screen.getByRole("button", { name: "Reject" });
  expect(reject.classList.contains("btn--secondary")).toBe(true);
  fireEvent.click(reject);
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: null });
  expect(screen.queryByLabelText("Reason (optional)")).toBeNull();
  // Pending until the computer takes it; Allow can't be tapped meanwhile.
  expect(screen.getByRole("button", { name: "Sending" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", true);
  await act(async () => { settle(); });
  expect(screen.getByRole("status").textContent).toBe("Sent. Updating…");
});

it("says Not now to a request for more access in one tap, and Allow still waits for the computer", async () => {
  const base = approvals(Date.now())[0]!;
  const more: Approval = { ...base, kind: "other", title: "Claude Code needs to change files in Home.", headline: undefined, facts: undefined,
    scope: "Turns on, for this chat: Change files in Home. Nothing else changes.", access_request: { needs: ["change_files"], reason: "to create marker.txt" } };
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const { unmount } = render(<ApprovalCard approval={more} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Not now" })); });
  expect(onDecide).toHaveBeenCalledTimes(1);
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: null });
  unmount();
  let settle!: () => void;
  const slow = vi.fn(() => new Promise<void>(done => { settle = done; }));
  render(<ApprovalCard approval={more} agent="Claude Code" myDeviceId="phone" canDecide onDecide={slow} />);
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  expect(slow).toHaveBeenCalledWith({ decision: "allow" });
  // Never shown as allowed before the computer says so.
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.getByRole("button", { name: "Sending" })).toBeTruthy();
  await act(async () => { settle(); });
  expect(screen.getByRole("status").textContent).toBe("Sent. Updating…");
});

it("puts Show command and the reason on one line under the buttons, names unchanged", () => {
  const approval = approvals(Date.now())[0]!;
  render(<ApprovalCard approval={approval} agent="Claude Code" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  const reason = screen.getByRole("button", { name: "Reject with a reason" });
  const more = document.querySelector<HTMLDetailsElement>("details.approval__more")!;
  const line = reason.parentElement!;
  expect(line.classList.contains("approval__meta")).toBe(true);
  expect(more.parentElement).toBe(line);
  // The request's toggle comes first, so opening it leaves the toggle where it was.
  expect([...line.children].map(c => c.tagName)).toEqual(["DETAILS", "BUTTON"]);
  expect(more.querySelector("summary")!.textContent).toBe(approval.kind === "command" ? "Show command" : "Show details");
  expect(screen.getByRole("button", { name: "Allow" }).compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

it("shows what a request allows in the open when there is nothing more to show", () => {
  const approval: Approval = { ...approvals(Date.now())[0]!, detail: "", detail_lines: undefined, raw_request: null, facts: [] };
  render(<ApprovalCard approval={approval} agent="Claude Code" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.querySelector("details.approval__more")).toBeNull();
  expect(screen.getByText("This request once").closest(".approval__meta")).toBeNull();
  expect(screen.getByRole("button", { name: "Reject with a reason" }).parentElement!.classList.contains("approval__meta")).toBe(true);
});

it("keeps a reason one quiet tap away, where the refusal becomes the primary and Back returns", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  fireEvent.click(screen.getByRole("button", { name: "Reject with a reason" }));
  expect(onDecide).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reject with a reason" }));
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  const deny = screen.getByRole("button", { name: /^Reject$/ });
  expect(deny.classList.contains("btn--primary")).toBe(true);
  fireEvent.change(screen.getByLabelText("Reason (optional)"), { target: { value: "Wait for the maintainer" } });
  await act(async () => { fireEvent.click(deny); });
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: "Wait for the maintainer" });
});

import type { Approval } from "../protocol/types";
it.each(cases.filter(c => !("questions" in c.approval)))("reads $tool at a glance, with the decision above the request itself", ({ approval, input }) => {
  render(<ApprovalCard approval={{ ...approvals(Date.now())[0]!, headline: undefined, facts: undefined, ...approval, kind: approval.kind as Approval["kind"], raw_request: JSON.stringify(input) }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  // The agent's own account heads it, else what the computer calls it.
  expect(screen.getByRole("heading", { name: plainToolNames(approval.headline ?? approval.title) })).toBeTruthy();
  const glance = document.querySelector(".approval__glance");
  if (approval.facts?.length) {
    const said = approval.facts.map(f => f.text).join(", ");
    expect(glance!.textContent).toBe(said.charAt(0).toUpperCase() + said.slice(1));
  } else {
    expect(glance).toBeNull();
  }
  // Decline and Allow come before the request, which is one tap away.
  const allow = screen.getByRole("button", { name: "Allow" });
  const more = document.querySelector<HTMLDetailsElement>("details.approval__more")!;
  expect(more.open).toBe(false);
  expect(allow.compareDocumentPosition(more) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(more.querySelector("summary")!.textContent).toBe(approval.kind === "command" ? "Show command" : "Show details");
  const detail = more.querySelector(".approval__detail");
  expect(detailLines(detail!)).toEqual(approval.detail_lines!.map(line => ({ text: line.label ? `${line.label}: ${line.text}` : line.text, code: line.code && line.change !== "omitted" ? line.text : null, change: line.change ?? null })));
  expect(detailLines(detail!).map(line => line.change ? `${PLAIN_MARK[line.change]} ${line.text}` : line.text).join("\n")).toBe(approval.detail);
  expect(within(more).getByText("This request once")).toBeTruthy();
  expect(screen.queryByText("Shortened · the full request is under Exact request") !== null).toBe(approval.shortened);
  act(() => { more.open = true; more.dispatchEvent(new Event("toggle")); });
  expect(more.querySelector("summary")!.textContent).toBe(approval.kind === "command" ? "Hide command" : "Hide details");
});

it("marks the risky part of what a request does, in words as well as colour", () => {
  const approval: Approval = { ...approvals(Date.now())[0]!, headline: "Clean and publish", facts: [{ text: "deletes target" , risky: true }, { text: "makes a git commit" }, { text: "pushes to origin", risky: true }] };
  render(<ApprovalCard approval={approval} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const glance = document.querySelector(".approval__glance")!;
  expect(glance.textContent).toBe("Check this: Deletes target, makes a git commit, Check this: pushes to origin");
  expect([...glance.querySelectorAll("mark")].map(m => m.textContent)).toEqual(["Check this: Deletes target", "Check this: pushes to origin"]);
  expect(glance.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
});

it("shows the start of the request where an older computer says nothing about what it does", () => {
  const approval = approvals(Date.now())[0]!;
  const { headline: _, facts: __, ...older } = approval;
  render(<ApprovalCard approval={{ ...older, detail: "cargo test\nRun the tests", detail_lines: [{ text: "cargo test", code: true }, { text: "Run the tests" }] }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.getByRole("heading", { name: "Run a shell command" })).toBeTruthy();
  expect(document.querySelector(".approval__peek")!.textContent).toContain("cargo test");
  expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
});

it("draws the buttons it is given, which act through the card", async () => {
  const act1 = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}}
    actions={(ctx) => <>
      <button type="button" disabled={ctx.disabled} onClick={() => void ctx.run("other", act1)}>{ctx.sending === "other" ? "Going" : "Another way"}</button>
      <button type="button" onClick={ctx.decline}>No</button>
    </>} />);
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Another way" })); });
  expect(act1).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("status").textContent).toBe("Sent. Updating…");
});

it("sits in a list row without a frame or heading row of its own", () => {
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId={null} canDecide embedded onDecide={async () => {}} />);
  expect(document.querySelector(".approval--embedded")).toBeTruthy();
  expect(document.querySelector(".approval__head")).toBeNull();
});

it("keeps verbose facts and a long request behind a tap, below both decisions", () => {
  const base = approvals(Date.now())[0]!;
  const fact = { text: "deletes a long list of files outside the selected project ".repeat(4), risky: true };
  const headline = "A request with a very long explanation of the action and its target ".repeat(3);
  render(<ApprovalCard approval={{ ...base, headline, facts: [fact], detail: "Detailed explanation ".repeat(40), detail_lines: undefined, raw_request: null }} agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  const card = document.querySelector(".approval")!;
  expect(card.querySelector(".approval__title")!.textContent!.length).toBeLessThan(headline.length);
  // What needs attention stays at a glance, three lines of it at most.
  const glance = card.querySelector(".approval__glance")!;
  expect(glance.textContent!.toLowerCase()).toContain(fact.text.trim());
  expect(glance.querySelector(".approval__glance-text--clamped")).toBeTruthy();
  const more = card.querySelector<HTMLDetailsElement>("details.approval__more")!;
  expect(more.open).toBe(false);
  expect(more.querySelector(".approval__full-title")!.textContent).toBe(headline);
  expect(more.textContent!.toLowerCase()).toContain(fact.text.trim());
  expect(more.textContent).toContain("Detailed explanation");
  for (const name of ["Allow", "Reject"]) {
    expect(screen.getByRole("button", { name }).compareDocumentPosition(more) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  }
  fireEvent.click(more.querySelector("summary")!);
  expect(more.open).toBe(true);
});

it("keeps only what needs attention at a glance when the computer's account is long, and all of it in the details", () => {
  const base = approvals(Date.now())[0]!;
  const facts = [
    { text: "reads every file under the project's source folder and its tests" },
    { text: "deletes the build output folder outside the project", risky: true },
    { text: "runs the formatter over each changed file in place" },
    { text: "pushes the branch to origin", risky: true },
  ];
  render(<ApprovalCard approval={{ ...base, facts }} agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  const [glance, full] = [...document.querySelectorAll(".approval__glance")];
  expect(glance!.closest("details")).toBeNull();
  expect(glance!.textContent).toBe("Check this: Deletes the build output folder outside the project, Check this: pushes the branch to origin");
  expect(glance!.classList.contains("approval__glance--risky")).toBe(true);
  expect(glance!.querySelector(".approval__glance-text--clamped")).toBeTruthy();
  // Every check, in the computer's order, one tap away.
  expect(full!.closest("details.approval__more")).toBeTruthy();
  const said = facts.map(f => f.risky ? `Check this: ${f.text}` : f.text).join(", ");
  expect(full!.textContent).toBe(said.charAt(0).toUpperCase() + said.slice(1));
  expect(document.body.textContent).not.toMatch(/needing attention|checks, including/);
});

it("shows every check of a long account, unmarked, when none needs attention", () => {
  const base = approvals(Date.now())[0]!;
  const facts = [
    { text: "reads every file under the project's source folder and its tests" },
    { text: "runs the formatter over each changed file in place" },
    { text: "writes a summary of the results to notes.md in the project" },
  ];
  render(<ApprovalCard approval={{ ...base, facts }} agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  const glance = document.querySelector(".approval__glance")!;
  const said = facts.map(f => f.text).join(", ");
  expect(glance.textContent).toBe(said.charAt(0).toUpperCase() + said.slice(1));
  expect(glance.classList.contains("approval__glance--risky")).toBe(false);
  expect(glance.querySelector("svg, mark")).toBeNull();
  expect(glance.querySelector(".approval__glance-text--clamped")).toBeTruthy();
  expect(document.querySelector("details.approval__more .approval__glance")!.textContent).toBe(glance.textContent);
  expect(document.body.textContent).not.toMatch(/needing attention|checks, including/);
});

it("shows a short account whole and unclamped, in the computer's order", () => {
  const base = approvals(Date.now())[0]!;
  const facts = [{ text: "changes notes.md" }, { text: "pushes to origin", risky: true }, { text: "makes a git commit" }];
  render(<ApprovalCard approval={{ ...base, facts }} agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  const glances = document.querySelectorAll(".approval__glance");
  expect(glances).toHaveLength(1);
  expect(glances[0]!.textContent).toBe("Changes notes.md, Check this: pushes to origin, makes a git commit");
  expect(glances[0]!.querySelector(".approval__glance-text--clamped")).toBeNull();
});

it("retains verbose facts even without a host detail or raw request", () => {
  const base = approvals(Date.now())[0]!;
  const fact = { text: "dangerous target ".repeat(12), risky: true };
  render(<ApprovalCard approval={{ ...base, facts: [fact], detail: null, raw_request: null }} agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.querySelector(".approval__more")!.textContent!.toLowerCase()).toContain(fact.text.trim());
});

/** How the companion's plain detail marks each line of a proposed change. */
const PLAIN_MARK: Record<string, string> = { unchanged: " ", removed: "-", added: "+", omitted: "⋯" };

/**
 * Each shown line, the part of it set as code, and for a line of a proposed
 * change how it changes, read from what is seen rather than what is said.
 */
function detailLines(root: Element) {
  return [...root.querySelectorAll(".approval__line:not(.approval__change), .approval__change-line")].map(line => {
    const change = [...line.classList].find(name => name.startsWith("approval__change-line--"))?.slice("approval__change-line--".length) ?? null;
    const seen = line.cloneNode(true) as Element;
    seen.querySelectorAll(".visually-hidden, .approval__change-mark").forEach(hidden => hidden.remove());
    const code = seen.matches("code") ? seen : seen.querySelector("code");
    return { text: seen.textContent, code: code?.textContent ?? null, change };
  });
}

const change = cases.find(c => c.input && (c.input as { file_path?: string }).file_path === "client/src/screens/Home.tsx")!;

it("shows an edit's proposed change as marked lines, said as removed and added, and leaves it out of a brief row", () => {
  const approval: Approval = { ...approvals(Date.now())[0]!, ...change.approval, kind: "file_edit", raw_request: JSON.stringify(change.input) };
  const { unmount } = render(<ApprovalCard approval={approval} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const group = screen.getByRole("group", { name: "Proposed change" });
  const lines = [...group.querySelectorAll(".approval__change-line")];
  const marks = lines.map(line => line.querySelector(".approval__change-mark")!);
  expect(marks.every(mark => mark.getAttribute("aria-hidden") === "true")).toBe(true);
  const said = (kind: string) => lines.filter(line => line.classList.contains(`approval__change-line--${kind}`));
  expect(said("removed").length).toBeGreaterThan(0);
  expect(said("added").length).toBeGreaterThan(0);
  for (const line of said("removed")) {
    expect(line.querySelector(".approval__change-mark")!.textContent).toBe("−");
    expect(line.querySelector("code .visually-hidden")!.textContent).toBe("Removed: ");
  }
  for (const line of said("added")) {
    expect(line.querySelector(".approval__change-mark")!.textContent).toBe("+");
    expect(line.querySelector("code .visually-hidden")!.textContent).toBe("Added: ");
  }
  for (const line of said("unchanged")) expect(line.querySelector(".visually-hidden")).toBeNull();
  expect(group.textContent).toContain("Removed:   if (!tasks.length) return <Empty />;");
  expect(group.textContent).toContain('Added:   if (!tasks.length) return <Empty title="Nothing waiting" />;');
  unmount();

  render(<p><ApprovalDetailText approval={approval} brief /></p>);
  expect(document.querySelector(".approval__change")).toBeNull();
  expect(document.body.textContent).toContain("client/src/screens/Home.tsx");
});

it("keeps a command's own line breaks and spacing, and leaves a detail without code as prose", () => {
  const approval = approvals(Date.now())[0]!;
  const { unmount } = render(<ApprovalCard approval={{ ...approval, detail: "cd crates\n  cargo test\nRun the tests", detail_lines: [{ text: "cd crates\n  cargo test", code: true }, { text: "Run the tests" }] }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(detailLines(document.querySelector(".approval__detail")!)).toEqual([{ text: "cd crates\n  cargo test", code: "cd crates\n  cargo test", change: null }, { text: "Run the tests", code: null, change: null }]);
  unmount();
  render(<ApprovalCard approval={{ ...approval, kind: "other", detail: "Needs the network to fetch crates.", detail_lines: [] }} agent="Codex" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const detail = document.querySelector(".approval__detail")!;
  expect(detail.textContent).toBe("Needs the network to fetch crates.");
  expect(detail.querySelector("code, .approval__line")).toBeNull();
});

const asked = cases.find(c => c.tool === "AskUserQuestion")!;
const question = (): Approval => ({ ...approvals(Date.now())[0]!, ...asked.approval, kind: "question", raw_request: JSON.stringify(asked.input) });

it("shows each question with its options and what they mean, and sends the chosen answers", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: "Answer 2 questions" })).toBeTruthy();
  expect(document.querySelector(".approval__detail")).toBeNull();
  const [rounding, reports] = document.querySelectorAll("fieldset");
  expect(rounding!.querySelector("legend")!.textContent).toBe("RoundingHow should totals be rounded?");
  expect(reports!.querySelector("legend")!.textContent).toBe("ReportsWhich reports need the change?Choose one or more");
  const nearest = screen.getByRole("radio", { name: "Nearest cent" });
  expect(document.getElementById(nearest.getAttribute("aria-describedby")!)!.textContent).toBe("Round half up to two decimal places");
  expect(screen.getAllByRole("radio").map(r => r.getAttribute("name"))).toEqual([nearest.getAttribute("name"), nearest.getAttribute("name")]);
  expect(screen.getAllByRole("checkbox").map(c => c.closest("label")!.textContent)).toEqual(["Invoices", "Receipts", "StatementsMonthly account statements"]);
  expect(screen.getByRole("checkbox", { name: "Invoices" }).getAttribute("aria-describedby")).toBeNull();
  const send = screen.getByRole("button", { name: "Send answer" });
  expect(send).toHaveProperty("disabled", true);

  fireEvent.click(screen.getByRole("radio", { name: "Always up" }));
  expect(send).toHaveProperty("disabled", true);
  fireEvent.click(screen.getByRole("checkbox", { name: "Statements" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Invoices" }));
  fireEvent.change(screen.getByLabelText("Add your own answer"), { target: { value: " Only paid ones " } });
  expect(send).toHaveProperty("disabled", false);
  await act(async () => { fireEvent.click(send); });
  expect(onDecide).toHaveBeenCalledWith({
    decision: "answer",
    text: "Rounding: Always up; Reports: Invoices, Statements, Only paid ones",
    answers: [
      { question: "How should totals be rounded?", choices: ["Always up"] },
      { question: "Which reports need the change?", choices: ["Invoices", "Statements", "Only paid ones"] },
    ],
  });
});

it("takes one option or the person's own words for a single-choice question, never both", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const one = { ...question(), title: "How should totals be rounded?", questions: question().questions!.slice(0, 1) };
  render(<ApprovalCard approval={one} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: "How should totals be rounded?" })).toBeTruthy();
  // The heading is the question; its header and words stay for a screen reader only.
  expect([...document.querySelectorAll("fieldset legend .visually-hidden")].map(e => e.textContent)).toEqual(["Rounding", "How should totals be rounded?"]);
  const nearest = screen.getByRole("radio", { name: "Nearest cent" });
  const own = screen.getByLabelText("Or answer in your own words");
  fireEvent.click(nearest);
  fireEvent.change(own, { target: { value: "Nearest, but never below cost" } });
  expect(nearest).toHaveProperty("checked", false);
  fireEvent.click(nearest);
  expect(own).toHaveProperty("value", "");
  fireEvent.change(own, { target: { value: "Nearest, but never below cost" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send answer" })); });
  expect(onDecide).toHaveBeenCalledWith({
    decision: "answer",
    text: "Nearest, but never below cost",
    answers: [{ question: "How should totals be rounded?", choices: ["Nearest, but never below cost"] }],
  });
});

it("asks a secret hidden and never reads the answer back", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const secret = { ...question(), title: "What's the staging password?", questions: [{ question: "What's the staging password?", options: [], secret: true }] };
  const { unmount } = render(<ApprovalCard approval={secret} agent="Codex" myDeviceId={null} canDecide onDecide={onDecide} />);
  const typed = screen.getByLabelText("Type it");
  expect(typed.getAttribute("type")).toBe("password");
  fireEvent.change(typed, { target: { value: "hunter2" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send answer" })); });
  const answers = [{ question: "What's the staging password?", choices: ["hunter2"] }];
  expect(onDecide).toHaveBeenCalledWith({ decision: "answer", text: "hunter2", answers });
  unmount();
  render(<ApprovalCard approval={{ ...secret, state: "allowed", resolved_by: { kind: "device", device_id: "phone" }, resolved_at: Date.now(),
    delivery: { decision: { decision: "answer", text: "hunter2", answers }, at: Date.now(), error: null } }}
  agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.body.textContent).not.toContain("hunter2");
  expect([...document.querySelectorAll(".approval__settled-what")].map(p => p.textContent)).toContain("Answered");
});

it("declines a question without sending a reason it never asked for", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Decline" })); });
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: null });
});

it("settles as Answered with the answer given, not Allowed", () => {
  const answers = [
    { question: "How should totals be rounded?", choices: ["Always up"] },
    { question: "Which reports need the change?", choices: ["Invoices", "Statements"] },
  ];
  render(<ApprovalCard approval={{ ...question(), state: "allowed", resolved_by: { kind: "device", device_id: "phone" }, resolved_at: Date.now(),
    delivery: { decision: { decision: "answer", text: "Rounding: Always up; Reports: Invoices, Statements", answers }, at: Date.now(), error: null } }}
  agent="Claude Code" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.querySelector(".approval__settled-title")!.textContent).toMatch(/^Answered · /);
  expect(screen.getByText(/^Answered by you\. Sent to the agent/)).toBeTruthy();
  expect([...document.querySelectorAll(".approval__settled-what")].map(p => p.textContent)).toEqual([
    "Answer 2 questions", "Rounding: Always up", "Reports: Invoices, Statements",
  ]);
  expect(screen.queryByRole("radio")).toBeNull();
  expect(screen.queryByRole("checkbox")).toBeNull();
  // Keep the question's prose, not its old option descriptions or controls.
  expect([...document.querySelectorAll(".approval__detail .approval__line")].map(p => p.textContent)).toEqual([
    "How should totals be rounded?", "Which reports need the change?",
  ]);
  expect(screen.queryByText("Round half up to two decimal places")).toBeNull();
});

it("makes Send answer the primary on a question, saying what it waits for, and Decline quiet", () => {
  const { unmount } = render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const send = screen.getByRole("button", { name: "Send answer" });
  const decline = screen.getByRole("button", { name: "Decline" });
  expect(send.classList.contains("btn--primary")).toBe(true);
  expect(decline.classList.contains("btn--quiet")).toBe(true);
  expect(send).toHaveProperty("disabled", true);
  expect(document.getElementById(send.getAttribute("aria-describedby")!)!.textContent).toBe("Answer each question to send.");
  fireEvent.click(screen.getByRole("radio", { name: "Always up" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Invoices" }));
  expect(send).toHaveProperty("disabled", false);
  expect(send.getAttribute("aria-describedby")).toBeNull();
  expect(screen.queryByText("Answer each question to send.")).toBeNull();
  unmount();
  // Asked in words only: the answer is typed.
  render(<ApprovalCard approval={{ ...question(), questions: [], detail: "Which branch should I use?" }} agent="Codex" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const typed = screen.getByRole("button", { name: "Send answer" });
  expect(document.getElementById(typed.getAttribute("aria-describedby")!)!.textContent).toBe("Write an answer to send it.");
  fireEvent.change(screen.getByLabelText("Your answer"), { target: { value: "main" } });
  expect(typed).toHaveProperty("disabled", false);
});

it("keeps focus on the answered request, or moves it to the next one waiting on the same screen", async () => {
  const first = approvals(Date.now())[0]!;
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const { unmount } = render(<ApprovalCard approval={first} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  const allow = screen.getByRole("button", { name: "Allow" });
  allow.focus();
  await act(async () => { fireEvent.click(allow); });
  expect(document.activeElement).toBe(document.querySelector("article.approval"));
  unmount();

  render(<div className="route">
    <ApprovalCard approval={first} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />
    <ApprovalCard approval={{ ...first, approval_id: `${first.approval_id}-next` }} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />
  </div>);
  const [card, next] = [...document.querySelectorAll<HTMLElement>("article.approval")];
  await act(async () => { fireEvent.click(within(card!).getByRole("button", { name: "Allow" })); });
  expect(document.activeElement).toBe(next);
  expect(card!.hasAttribute("data-open-decision")).toBe(false);
});

it("gives a screen reader the deadline as one value, not a clock time and parentheses read apart", () => {
  const expires = Date.now() + 8 * 60_000 + 30_000;
  render(<ApprovalCard approval={{ ...approvals(Date.now())[0]!, expires_at: expires }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const value = screen.getByText(/^Answer by/);
  expect(value.childNodes).toHaveLength(2);
  const time = value.lastElementChild!;
  expect(time.tagName).toBe("TIME");
  expect(time.textContent).toBe(`${clockTime(expires)} (in 8 min)`);
  expect(time.getAttribute("aria-label")).toBe(`${clockTime(expires)}, in 8 minutes`);
  expect(time.getAttribute("dateTime")).toBe(new Date(expires).toISOString());
});

it("shows the deadline once, as one line under the buttons, and nothing for a request that has none", () => {
  const expires = Date.now() + 9 * 60_000;
  const { unmount } = render(<ApprovalCard approval={{ ...approvals(Date.now())[0]!, expires_at: expires }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.getAllByText(clockTime(expires), { exact: false })).toHaveLength(1);
  expect(screen.queryByText("Expires")).toBeNull();
  const line = screen.getByText(/^Answer by/);
  expect(screen.getByRole("button", { name: "Allow" }).compareDocumentPosition(line) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  // What the request allows is still in the details.
  expect(within(document.querySelector("details.approval__more")!).getByText("This request once")).toBeTruthy();
  unmount();
  const q = render(<ApprovalCard approval={{ ...question(), expires_at: expires }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.getAllByText(clockTime(expires), { exact: false })).toHaveLength(1);
  expect(screen.getByText(/^Answer by/)).toBeTruthy();
  q.unmount();
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.queryByText(/^Answer by/)).toBeNull();
});

describe("a question card is just the question", () => {
  const visibleText = () => {
    const card = document.querySelector("article.approval")!.cloneNode(true) as HTMLElement;
    card.querySelectorAll(".visually-hidden").forEach(e => e.remove());
    return card.textContent!;
  };
  const noFurniture = () => {
    expect(visibleText()).not.toContain("wants to ask you");
    expect(screen.queryByText("Allows")).toBeNull();
    expect(screen.queryByText("Only your answer; nothing runs")).toBeNull();
    expect(screen.queryByText("Exact request")).toBeNull();
    // The greyed Send carries why it waits; no line under it says so.
    expect(document.querySelector(".approval__note")).toBeNull();
    expect(screen.getByRole("button", { name: "Decline" }).classList.contains("btn--quiet")).toBe(true);
  };

  it("one question with one choice: the heading, its options, own words and Send", () => {
    const one = { ...question(), title: "How should totals be rounded?", questions: question().questions!.slice(0, 1) };
    render(<ApprovalCard approval={one} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
    noFurniture();
    expect(visibleText().match(/How should totals be rounded\?/g)).toHaveLength(1);
    expect(screen.getAllByRole("radio")).toHaveLength(2);
    const own = screen.getByLabelText("Or answer in your own words");
    expect(own.getAttribute("placeholder")).toBe("Or answer in your own words");
    expect(screen.getByRole("button", { name: "Send answer" })).toHaveProperty("disabled", true);
  });

  it("several choices: says one or more may be chosen", () => {
    const many = { ...question(), title: "Which reports need the change?", questions: question().questions!.slice(1) };
    render(<ApprovalCard approval={many} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
    noFurniture();
    expect(screen.getByText("Choose one or more")).toBeTruthy();
    expect(screen.getAllByRole("checkbox")).toHaveLength(3);
    expect(screen.getByLabelText("Add your own answer").getAttribute("placeholder")).toBe("Add your own answer");
  });

  it("several questions: each keeps its header and words", () => {
    render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
    noFurniture();
    expect(screen.getByRole("heading", { name: "Answer 2 questions" })).toBeTruthy();
    for (const said of ["Rounding", "How should totals be rounded?", "Reports", "Which reports need the change?"])
      expect(visibleText()).toContain(said);
  });

  it("own words only: one field that says what it is for, and Send", async () => {
    const onDecide = vi.fn().mockResolvedValue(undefined);
    const words = { ...question(), title: "What should the new branch be called?", questions: [{ header: "Name", question: "What should the new branch be called?", options: [] }] };
    render(<ApprovalCard approval={words} agent="Codex" myDeviceId={null} canDecide onDecide={onDecide} />);
    noFurniture();
    const field = screen.getByLabelText("Your answer");
    expect(field.getAttribute("placeholder")).toBe("Type your answer");
    fireEvent.change(field, { target: { value: "fix/parser" } });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send answer" })); });
    expect(onDecide).toHaveBeenCalledWith({ decision: "answer", text: "fix/parser", answers: [{ question: "What should the new branch be called?", choices: ["fix/parser"] }] });
  });

  it("keeps the exact request when the question was cut short, and who asked where the card opens its task", () => {
    const { unmount } = render(<ApprovalCard approval={{ ...question(), shortened: true }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
    expect(screen.getByText("Exact request")).toBeTruthy();
    unmount();
    render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onOpenTask={() => {}} onDecide={async () => {}} />);
    expect(screen.getByRole("button", { name: "Claude Code" })).toBeTruthy();
  });
});

// These same fixtures are checked against the native adapter's mapped event
// and response, so a phone answer must satisfy both sides of the contract.
import codexQuestions from "../../../crates/harness-codex/test-fixtures/questions.json";
it.each(codexQuestions)("answers Codex $name questions through the mapped approval", async (fixture) => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const approval: Approval = { ...approvals(Date.now())[0]!, ...fixture.approval, kind: "question", raw_request: JSON.stringify(fixture.input) };
  const { unmount } = render(<ApprovalCard approval={approval} agent="Codex" myDeviceId="phone" canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: fixture.approval.title })).toBeTruthy();
  const send = screen.getByRole("button", { name: "Send answer" });
  expect(send).toHaveProperty("disabled", true);
  const fields = document.querySelectorAll("fieldset");
  for (const [i, q] of fixture.approval.questions.entries()) {
    expect(fields[i]!.querySelector("legend")!.textContent).toContain(q.question);
    const choice = fixture.decision.answers[i]!.choices[0]!;
    if (q.options.some(o => o.label === choice)) {
      fireEvent.click(screen.getByRole("radio", { name: choice }));
    } else {
      fireEvent.change(fields[i]!.querySelector("textarea")!, { target: { value: choice } });
    }
    expect(send).toHaveProperty("disabled", i < fields.length - 1);
  }
  await act(async () => { fireEvent.click(send); });
  expect(onDecide).toHaveBeenCalledWith(fixture.decision);
  unmount();
  render(<ApprovalCard approval={{ ...approval, state: "allowed", resolved_at: Date.now(), resolved_by: { kind: "device", device_id: "phone" },
    delivery: { decision: { ...fixture.decision, decision: "answer" }, at: Date.now(), error: null } }}
    agent="Codex" myDeviceId="phone" canDecide onDecide={onDecide} />);
  for (const [i, answer] of fixture.decision.answers.entries()) {
    const prefix = fixture.decision.answers.length > 1 ? `${fixture.approval.questions[i]!.header}: ` : "Your answer: ";
    expect(screen.getByText(prefix + answer.choices.join(", "))).toBeTruthy();
  }
});

it("keeps answers with repeated wording under their own headers and hides the secret one", () => {
  const questions = [
    { id: "public", header: "Name", question: "Which value?", options: [] },
    { id: "secret", header: "Password", question: "Which value?", options: [], secret: true },
  ];
  const answers = [
    { id: "secret", question: "Which value?", choices: ["private-value"] },
    { id: "public", question: "Which value?", choices: ["public-name"] },
  ];
  render(<ApprovalCard approval={{ ...question(), questions, state: "allowed", resolved_at: Date.now(),
    resolved_by: { kind: "device", device_id: "phone" },
    delivery: { decision: { decision: "answer", text: "public-name; private-value", answers }, at: Date.now(), error: null } }}
    agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(screen.getByText("Name: public-name")).toBeTruthy();
  expect(screen.getByText("Password: answered")).toBeTruthy();
  expect(document.body.textContent).not.toContain("private-value");
});

it("names another server's tool in words in a request to use it, before and after it is answered", () => {
  const use: Approval = { ...approvals(Date.now())[0]!, kind: "tool", title: "Use mcp__github__create_issue", headline: undefined, facts: undefined };
  const { unmount } = render(<ApprovalCard approval={use} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.getByRole("heading", { name: "Create issue (github)" })).toBeTruthy();
  unmount();
  render(<ApprovalCard approval={{ ...use, state: "denied", resolved_by: { kind: "device", device_id: "phone" }, resolved_at: Date.now() }} agent="Claude Code" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.body.textContent).not.toContain("mcp__");
});
